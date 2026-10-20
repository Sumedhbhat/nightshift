//! Measurement helpers for the release-build performance scenario. No product state lives here.
use serde::Serialize;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Summary of a set of durations, in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Distribution {
    pub count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl Distribution {
    pub fn from_durations(samples: &[Duration]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        sorted.sort_by(f64::total_cmp);
        let rank = |p: f64| {
            let index = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
            round(sorted[index.clamp(1, sorted.len()) - 1])
        };
        Self {
            count: sorted.len(),
            p50_ms: rank(50.0),
            p95_ms: rank(95.0),
            p99_ms: rank(99.0),
            max_ms: round(sorted[sorted.len() - 1]),
        }
    }
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Frame intervals longer than this multiple of the nominal period count as missed frames.
pub const MISSED_FRAME_FACTOR: f64 = 1.5;

pub fn missed_frames(intervals: &[Duration], period: Duration) -> usize {
    let limit = period.as_secs_f64() * MISSED_FRAME_FACTOR;
    intervals
        .iter()
        .filter(|interval| interval.as_secs_f64() > limit)
        .count()
}

/// Resident set size and its peak, in KiB, for the current process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Memory {
    pub rss_kib: u64,
    pub peak_kib: Option<u64>,
}

#[cfg(target_os = "linux")]
pub fn memory() -> Option<Memory> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse().ok())
    };
    Some(Memory {
        rss_kib: field("VmRSS:")?,
        peak_kib: field("VmHWM:"),
    })
}

/// macOS has no procfs. `ps` reports current RSS; the scenario tracks the peak of its samples.
#[cfg(not(target_os = "linux"))]
pub fn memory() -> Option<Memory> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let rss_kib = String::from_utf8(output.stdout).ok()?.trim().parse().ok()?;
    Some(Memory {
        rss_kib,
        peak_kib: None,
    })
}

/// Whether memory stopped growing: compares the mean of the final quarter with the second quarter.
/// Growth under `tolerance` (a fraction) counts as a plateau.
pub fn plateau(samples: &[u64], tolerance: f64) -> Option<(f64, bool)> {
    if samples.len() < 8 {
        return None;
    }
    let quarter = samples.len() / 4;
    let mean = |slice: &[u64]| slice.iter().sum::<u64>() as f64 / slice.len() as f64;
    let middle = mean(&samples[quarter..2 * quarter]);
    let last = mean(&samples[samples.len() - quarter..]);
    let growth = (last - middle) / middle;
    Some((round(growth * 100.0), growth < tolerance))
}

/// User plus system CPU time consumed by this process so far.
#[cfg(target_os = "linux")]
pub fn cpu_time() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the parenthesized command name; utime and stime are fields 14 and 15.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: u64 = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    // USER_HZ is 100 on all supported Linux targets.
    Some(Duration::from_millis(ticks * 10))
}

#[cfg(not(target_os = "linux"))]
pub fn cpu_time() -> Option<Duration> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "cputime=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    // Formats: "M:SS.ss" or "H:MM:SS".
    let text = String::from_utf8(output.stdout).ok()?;
    let mut seconds = 0.0;
    for part in text.trim().split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(Duration::from_secs_f64(seconds))
}

pub fn unix_ms(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn elapsed_ms(since: Instant) -> f64 {
    round(since.elapsed().as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|v| Duration::from_millis(*v)).collect()
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let samples = ms(&(1..=100).collect::<Vec<_>>());
        let summary = Distribution::from_durations(&samples);
        assert_eq!(summary.count, 100);
        assert_eq!(summary.p50_ms, 50.0);
        assert_eq!(summary.p95_ms, 95.0);
        assert_eq!(summary.p99_ms, 99.0);
        assert_eq!(summary.max_ms, 100.0);
        assert_eq!(Distribution::from_durations(&[]), Distribution::default());
        assert_eq!(Distribution::from_durations(&ms(&[7])).p99_ms, 7.0);
    }

    #[test]
    fn missed_frames_count_long_intervals() {
        let period = Duration::from_micros(16_667);
        assert_eq!(missed_frames(&ms(&[16, 17, 24, 26, 40]), period), 2);
    }

    #[test]
    fn plateau_detects_growth() {
        let flat = vec![100; 40];
        assert_eq!(plateau(&flat, 0.05), Some((0.0, true)));
        let growing: Vec<u64> = (0..40).map(|i| 100 + i * 10).collect();
        assert!(!plateau(&growing, 0.05).unwrap().1);
        assert_eq!(plateau(&[1, 2, 3], 0.05), None);
    }

    #[test]
    fn current_process_cpu_time_is_readable() {
        let start = cpu_time().expect("read CPU time");
        let mut x = 0u64;
        for i in 0..50_000_000u64 {
            x = x.wrapping_add(std::hint::black_box(i));
        }
        std::hint::black_box(x);
        assert!(cpu_time().unwrap() >= start);
    }

    #[test]
    fn current_process_memory_is_readable() {
        let memory = memory().expect("read process memory");
        assert!(memory.rss_kib > 0);
    }
}
