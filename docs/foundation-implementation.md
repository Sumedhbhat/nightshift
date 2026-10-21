# Native foundation implementation

This starts milestone 1 of the [implementation plan](implementation-plan.md). Product scope remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md). The synthetic replay does not establish execution completion, authorize runs, or implement publication.

The synthetic screen described here is historical. The application now opens the [project and public issue scaffold](public-issue-discovery.md); replay remains a library test utility.

## Build and structure

- One Rust 2024 package, pinned to Rust 1.97.1.
- `gpui` and `gpui_platform` use Zed revision `5a9b9558db01a6b906cec2fb70a797affdc58cdd`, fetched from GitHub. `Cargo.lock` pins the transitive dependencies.
- The platform enables `font-kit` for macOS text rendering, following the [upstream README at the pinned revision](https://github.com/zed-industries/zed/blob/5a9b9558db01a6b906cec2fb70a797affdc58cdd/crates/gpui/README.md).
- The first build found Xcode's optional Metal compiler missing. The platform's supported `runtime_shaders` feature compiles the embedded shaders through Metal at launch instead. Include this cost in startup measurements; revisit precompiled shaders for distribution.
- The platform also enables `wayland` and `x11` for Linux release builds. The Zed workspace disables `gpui_linux` defaults, so without them a Linux build compiles but panics when it opens a window.
- `src/main.rs` owns one Tokio runtime with two worker threads. `src/replay.rs` owns synthetic producers and bounded output; `src/ui.rs` owns GPUI entities, focus, actions, and virtualized lists.
- A foreground GPUI task polls the Tokio channel every 33 milliseconds. It drains at most 256 messages per update. This small adapter avoids importing Zed's application infrastructure.

## Replay limits

Two producers each emit up to 100 events per second. Each event contains a stream ID, sequence number, and fixed 128-byte payload. The queue holds at most 256 events; each blocked producer holds at most one additional event. The cache retains the latest 2,048 blocks, discarding older synthetic data. The issue list generates visible fixtures on demand rather than allocating 10,000 issue descriptions.

Producer tasks use backpressure and skip missed timer ticks. Stopping replay drops its receiver and aborts both producers. Quitting also shuts down the runtime. Starting another replay resets producer sequence numbers while retaining the current cache and received count.

These are fixture limits, not production output storage rules. Real agent output must be spooled to storage and retained for inspection, with explicit failure handling when persistence cannot keep up. The replay does not yet cover paging 100,000 stored blocks.

## Validation

Reference machine: MacBook Air, Apple M3, 8 CPU cores, 16 GB RAM, macOS 26.5.2, built-in 2560 × 1664 display.

Debug and release builds succeeded from the pinned Git dependencies without a sibling-checkout dependency. Formatting and Clippy passed. The three replay tests cover bounded retention, stalled-consumer behavior, delivery from both producers, and producer cancellation.

Native window inspection verified text rendering, replay start/stop by button and Command-R, arrow-key selection during streaming, Tab/Return activation, and scrolling into rows above 9,960. It exposed a row-width issue that was fixed and visually checked in the release bundle. The UI displayed more than 4,000 received blocks while retaining 2,048. These are smoke checks, not frame-time measurements.

The first debug launch reached `Render` in 440 ms. One release-process RSS sample during replay was 95,536 KiB. Neither establishes a startup percentile or memory plateau. The dependency `block 0.1.6` emits a Rust future-compatibility warning; the current pinned toolchain builds successfully.

Use `sh scripts/bundle.sh --release` to create a local `.app` bundle. The app now initializes [local persistence](storage-implementation.md) off the UI thread and displays saved history counts separately from the fixtures.

## Performance validation

Measured on 2026-10-21 with release builds of commit `4913d3d`. Milestone 1 is complete on the evidence below.

### Method

`sh scripts/perf.sh [output] [seconds] [launches]` runs three measurements and writes JSON reports plus `summary.json`. Each uses temporary data directories.

1. **Startup.** It launches the real window ten times with `NIGHTSHIFT_STARTUP_REPORT`. Each launch records main entry to first render, first presented frame, and loaded projects, then quits. The script also measures process exec to first frame.
2. **Platform baseline.** For 30 seconds, `NIGHTSHIFT_PERF_BASELINE=1` renders only a status line while requesting a frame every vsync. This isolates the platform's frame pacing from Nightshift's views.
3. **Scenario.** `NIGHTSHIFT_PERF_REPORT` opens three virtualized lists for ten minutes. The first is 10,000 issue rows scrolled one row per frame. The second shows two replay streams at 100 events per second each, with 128-byte payloads, tailed live. The third is 100,000 stored output blocks in a temporary SQLite database, paged 100 at a time from the storage worker with at most 24 pages cached. It scrolls steadily and jumps every two seconds to force uncached loads. A timer simulates a keyboard selection change ten times per second.

Frame intervals are measured between consecutive renders while a frame is requested every vsync. Input latency runs from the simulated action to the next render. Memory is sampled once per second: `VmRSS`/`VmHWM` on Linux, and `ps` RSS on macOS. CPU is process user plus system time divided by wall time.

The `Performance` workflow runs the script on a clean `macos-15` runner when a `perf/**` branch is pushed.

### Machines

| Name | Hardware | OS and display |
| --- | --- | --- |
| macOS CI | GitHub `macos-15` runner, Apple M1 (Virtual), 3 cores, 7 GB | macOS 15.7.9, 60 Hz virtual display |
| Linux dev | AMD Ryzen 5 3500U with Radeon Vega 8, 8 threads, 5 GB | Ubuntu 26.04.1, GNOME Shell 50.1 run headless with a 1920×1080@60 virtual monitor |

The owner's MacBook Air M3 was not available during this run. Repeat the script there to compare with the earlier smoke checks above.

### Results

| Measure | Budget | macOS CI | Linux dev |
| --- | --- | --- | --- |
| Warm launch, exec to first frame, median of 9 | under 1 s | 170 ms | 241 ms |
| First launch after build | reported separately | 2.38 s | 3.02 s |
| Frame interval p50 / p95 / p99, scenario | p95 ≤ 16.7 ms | 16.67 / 18.68 / 20.25 ms | 47.2 / 49.1 / 49.9 ms |
| Frame interval p95, empty-window baseline | — | 18.67 ms | 38.1 ms |
| Missed frames, over 1.5× period | report | 191 of 35,871 (0.5%) | all, from compositor pacing |
| Action to next render p95 / max | p95 < 50 ms | 10.4 / 43.5 ms | 39.4 / 43.3 ms |
| Replay events received in 10 min | 120,000 offered | 119,159 | 119,996 |
| Stored page load p50 / p95 | — | 6.8 / 15.8 ms | 8.4 / 14.6 ms |
| Nightshift CPU, scenario | — | 21% of one core | 24% of one core |
| RSS first / peak / last | plateau | 57 / 66 / 66 MiB | 106 / 111 / 101 MiB |
| RSS growth, final quarter vs second | plateau | +1.0% | −8.3% |

### Assessment

- **Startup, input latency, large lists, sustained output, and memory meet their budgets.** RSS stays flat after the caches fill and does not grow with the 120,000 replay events or the 100,000 stored blocks.
- **macOS frame p95 is 18.7 ms, above the 16.7 ms budget.** The empty-window baseline on the same runner has the same p95, so the deviation is the virtual machine's frame pacing, not Nightshift's views. The scenario's median sits exactly on the 60 Hz period, with 0.5% missed frames.
- **Linux frame pacing is bounded by the headless compositor.** An empty GPUI window paces at 37 ms there, and the full scenario adds about 10 ms per frame. The local desktop session was locked, so a real display could not be used. This environment establishes no Linux frame budget.
- **The replay limits stand as the initial production bounds.** The queue holds 256 events and the cache 2,048 blocks. The largest drain per 33 ms update was 40 events on macOS, well under the 256-message cap. Decision D7 in [decisions.md](decisions.md) records the resulting output limits.

The first launch after a build includes Metal runtime-shader compilation on macOS and cold disk caches on both systems. It is reported separately, as planned.
