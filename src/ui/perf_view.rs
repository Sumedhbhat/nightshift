//! Release-build performance scenario. Enabled only by `NIGHTSHIFT_PERF_REPORT`; it uses a
//! temporary database and never touches application data.
use gpui::{
    App, Bounds, Context, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size, uniform_list,
};
use nightshift::{
    application::Coordinator,
    domain::{ExecutionChange, IssueSnapshot, NewExecution, NewProject, StoredOutput},
    perf::{self, Distribution},
    replay::{self, OutputCache, Replay},
    storage::Store,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    num::NonZeroU32,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};
use tokio::runtime::Handle;

const STORED_BLOCKS: usize = 100_000;
const PAGE: usize = 100;
/// Pages of stored output kept in UI memory. Older pages are evicted least-recently-requested.
const CACHED_PAGES: usize = 24;
const ISSUE_ROW: f32 = 44.;
const OUTPUT_ROW: f32 = 22.;

pub struct PerfConfig {
    pub report: PathBuf,
    /// Render only a status line, to measure platform frame pacing without Nightshift's views.
    pub baseline: bool,
    pub seconds: u64,
    pub refresh_hz: f64,
    pub main_started: Instant,
}

impl PerfConfig {
    pub fn from_env(main_started: Instant) -> Option<Self> {
        let report = std::env::var_os("NIGHTSHIFT_PERF_REPORT")?.into();
        let number = |name: &str, default: f64| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| *value > 0.0)
                .unwrap_or(default)
        };
        Some(Self {
            report,
            baseline: std::env::var_os("NIGHTSHIFT_PERF_BASELINE").is_some(),
            seconds: number("NIGHTSHIFT_PERF_SECONDS", 600.0) as u64,
            refresh_hz: number("NIGHTSHIFT_PERF_REFRESH_HZ", 60.0),
            main_started,
        })
    }
}

struct Stored {
    store: Store,
    execution: i64,
    directory: PathBuf,
    seed_ms: f64,
    pages: HashMap<usize, Vec<StoredOutput>>,
    recency: VecDeque<usize>,
    loading: HashSet<usize>,
    load_times: Vec<Duration>,
    max_cached_pages: usize,
}

struct PerfView {
    config: PerfConfig,
    runtime: Handle,
    first_render_ms: Option<f64>,
    first_frame_ms: Option<f64>,
    first_frame_unix_ms: Option<u128>,
    scenario_started: Option<Instant>,
    cpu_at_start: Option<Duration>,
    last_render: Option<Instant>,
    frames: Vec<Duration>,
    view_build: Vec<Duration>,
    input_latency: Vec<Duration>,
    pending_input: Option<Instant>,
    frame_index: usize,
    selected_issue: usize,
    issue_scroll: gpui::UniformListScrollHandle,
    replay_scroll: gpui::UniformListScrollHandle,
    stored_scroll: gpui::UniformListScrollHandle,
    replay: Option<Replay>,
    cache: OutputCache,
    max_drain: usize,
    stored: Option<Stored>,
    memory: Vec<(f64, u64)>,
    peak_kib: u64,
    status: String,
    finished: bool,
    _tasks: Vec<Task<()>>,
}

#[derive(Serialize)]
struct Report {
    version: &'static str,
    baseline: bool,
    os: &'static str,
    arch: &'static str,
    scenario_seconds: f64,
    /// Nightshift process CPU time during the scenario divided by its wall time.
    cpu_percent_of_one_core: Option<f64>,
    refresh_hz: f64,
    startup: Startup,
    frame_interval: Distribution,
    missed_frames: usize,
    view_build: Distribution,
    input_latency: Distribution,
    replay: ReplayReport,
    stored_output: StoredReport,
    memory: MemoryReport,
    error: Option<String>,
}

#[derive(Serialize)]
struct Startup {
    main_to_first_render_ms: Option<f64>,
    main_to_first_frame_ms: Option<f64>,
    first_frame_unix_ms: Option<u128>,
}

#[derive(Serialize)]
struct ReplayReport {
    streams: usize,
    events_per_second_each: usize,
    payload_bytes: usize,
    received: u64,
    cached: usize,
    cache_capacity: usize,
    queue_capacity: usize,
    max_drained_per_update: usize,
}

#[derive(Serialize)]
struct StoredReport {
    blocks: usize,
    seed_ms: Option<f64>,
    page_size: usize,
    cached_page_limit: usize,
    max_cached_pages: usize,
    page_load: Distribution,
}

#[derive(Serialize)]
struct MemoryReport {
    samples: Vec<(f64, u64)>,
    first_kib: Option<u64>,
    last_kib: Option<u64>,
    peak_kib: u64,
    final_quarter_growth_percent: Option<f64>,
    plateau: Option<bool>,
}

impl PerfView {
    fn new(config: PerfConfig, runtime: Handle, cx: &mut Context<Self>) -> Self {
        let directory =
            std::env::temp_dir().join(format!("nightshift-perf-{}", std::process::id()));
        let seed_directory = directory.clone();
        let seeding = runtime.spawn(async move { seed(seed_directory).await });
        let seed_task = cx.spawn(async move |this, cx| {
            let result = seeding.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok((store, execution, seed_ms))) => {
                        this.stored = Some(Stored {
                            store,
                            execution,
                            directory,
                            seed_ms,
                            pages: HashMap::new(),
                            recency: VecDeque::new(),
                            loading: HashSet::new(),
                            load_times: vec![],
                            max_cached_pages: 0,
                        });
                        this.start(cx);
                    }
                    Ok(Err(error)) => this.fail(format!("seeding failed: {error}"), cx),
                    Err(error) => this.fail(format!("seeding task failed: {error}"), cx),
                }
                cx.notify();
            });
        });
        Self {
            config,
            runtime,
            first_render_ms: None,
            first_frame_ms: None,
            first_frame_unix_ms: None,
            scenario_started: None,
            cpu_at_start: None,
            last_render: None,
            frames: Vec::with_capacity(64_000),
            view_build: Vec::with_capacity(64_000),
            input_latency: vec![],
            pending_input: None,
            frame_index: 0,
            selected_issue: 0,
            issue_scroll: gpui::UniformListScrollHandle::new(),
            replay_scroll: gpui::UniformListScrollHandle::new(),
            stored_scroll: gpui::UniformListScrollHandle::new(),
            replay: None,
            cache: OutputCache::new(),
            max_drain: 0,
            stored: None,
            memory: vec![],
            peak_kib: 0,
            status: format!("Seeding {STORED_BLOCKS} stored output blocks…"),
            finished: false,
            _tasks: vec![seed_task],
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.replay = Some(Replay::start(&self.runtime));
        self.scenario_started = Some(Instant::now());
        self.cpu_at_start = perf::cpu_time();
        self.status = format!("Running for {} seconds…", self.config.seconds);
        // Drain replay output on the foreground at the planned 33 ms cadence.
        self._tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(replay::UPDATE_INTERVAL)
                    .await;
                let Ok(running) = this.update(cx, |this, cx| {
                    if let Some(replay) = &mut this.replay {
                        let drained = replay.drain_into(&mut this.cache);
                        this.max_drain = this.max_drain.max(drained);
                        if drained > 0 {
                            this.replay_scroll.scroll_to_item(
                                this.cache.blocks.len().saturating_sub(1),
                                gpui::ScrollStrategy::Bottom,
                            );
                            cx.notify();
                        }
                    }
                    !this.finished
                }) else {
                    return;
                };
                if !running {
                    return;
                }
            }
        }));
        // Simulated keyboard selection, ten times per second.
        self._tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let Ok(running) = this.update(cx, |this, cx| {
                    this.selected_issue = (this.selected_issue + 1) % replay::ISSUE_COUNT;
                    this.pending_input.get_or_insert_with(Instant::now);
                    cx.notify();
                    !this.finished
                }) else {
                    return;
                };
                if !running {
                    return;
                }
            }
        }));
        // Memory samples once per second, off the UI thread.
        self._tasks.push(cx.spawn(async move |this, cx| {
            loop {
                let sample = cx
                    .background_executor()
                    .spawn(async { perf::memory() })
                    .await;
                let Ok(running) = this.update(cx, |this, _| {
                    if let (Some(sample), Some(started)) = (sample, this.scenario_started) {
                        let seconds = (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
                        this.memory.push((seconds, sample.rss_kib));
                        this.peak_kib = this
                            .peak_kib
                            .max(sample.rss_kib)
                            .max(sample.peak_kib.unwrap_or(0));
                    }
                    !this.finished
                }) else {
                    return;
                };
                if !running {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
        let seconds = self.config.seconds;
        self._tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(seconds))
                .await;
            let _ = this.update(cx, |this, cx| this.finish(None, cx));
        }));
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.status = error.clone();
        self.finish(Some(error), cx);
    }

    fn finish(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.replay = None;
        let report = self.report(error);
        let path = self.config.report.clone();
        let stored = self.stored.take();
        let closing = self.runtime.spawn(async move {
            if let Some(stored) = stored {
                let _ = stored.store.close().await;
                let _ = std::fs::remove_dir_all(&stored.directory);
            }
            let json = serde_json::to_string_pretty(&report).expect("serialize report");
            std::fs::write(&path, json)
        });
        self._tasks.push(cx.spawn(async move |_, cx| {
            if let Ok(Err(error)) = closing.await {
                eprintln!("nightshift: could not write performance report: {error}");
            }
            cx.update(|cx| cx.quit());
        }));
    }

    fn report(&self, error: Option<String>) -> Report {
        let samples: Vec<u64> = self.memory.iter().map(|(_, kib)| *kib).collect();
        let plateau = perf::plateau(&samples, 0.05);
        let period = Duration::from_secs_f64(1.0 / self.config.refresh_hz);
        Report {
            version: nightshift::VERSION,
            baseline: self.config.baseline,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            scenario_seconds: self
                .scenario_started
                .map_or(0.0, |started| perf::elapsed_ms(started) / 1000.0),
            cpu_percent_of_one_core: match (
                self.cpu_at_start,
                perf::cpu_time(),
                self.scenario_started,
            ) {
                (Some(start), Some(end), Some(started)) => Some(
                    ((end.saturating_sub(start)).as_secs_f64() / started.elapsed().as_secs_f64()
                        * 1000.0)
                        .round()
                        / 10.0,
                ),
                _ => None,
            },
            refresh_hz: self.config.refresh_hz,
            startup: Startup {
                main_to_first_render_ms: self.first_render_ms,
                main_to_first_frame_ms: self.first_frame_ms,
                first_frame_unix_ms: self.first_frame_unix_ms,
            },
            frame_interval: Distribution::from_durations(&self.frames),
            missed_frames: perf::missed_frames(&self.frames, period),
            view_build: Distribution::from_durations(&self.view_build),
            input_latency: Distribution::from_durations(&self.input_latency),
            replay: ReplayReport {
                streams: 2,
                events_per_second_each: 100,
                payload_bytes: 128,
                received: self.cache.received,
                cached: self.cache.blocks.len(),
                cache_capacity: replay::CACHE_CAPACITY,
                queue_capacity: replay::QUEUE_CAPACITY,
                max_drained_per_update: self.max_drain,
            },
            stored_output: StoredReport {
                blocks: STORED_BLOCKS,
                seed_ms: self.stored.as_ref().map(|s| s.seed_ms),
                page_size: PAGE,
                cached_page_limit: CACHED_PAGES,
                max_cached_pages: self.stored.as_ref().map_or(0, |s| s.max_cached_pages),
                page_load: Distribution::from_durations(
                    self.stored.as_ref().map_or(&[][..], |s| &s.load_times[..]),
                ),
            },
            memory: MemoryReport {
                first_kib: samples.first().copied(),
                last_kib: samples.last().copied(),
                peak_kib: self.peak_kib,
                final_quarter_growth_percent: plateau.map(|(growth, _)| growth),
                plateau: plateau.map(|(_, flat)| flat),
                samples: self.memory.clone(),
            },
            error,
        }
    }

    fn stored_row(&mut self, index: usize, cx: &mut Context<Self>) -> String {
        let page = index / PAGE;
        let Some(stored) = &mut self.stored else {
            return String::new();
        };
        if let Some(rows) = stored.pages.get(&page) {
            if let Some(position) = stored.recency.iter().position(|p| *p == page) {
                stored.recency.remove(position);
            }
            stored.recency.push_back(page);
            return rows
                .get(index % PAGE)
                .map(|row| format!("{:>6}  {}", row.sequence, row.payload))
                .unwrap_or_default();
        }
        if stored.loading.insert(page) {
            let store = stored.store.clone();
            let execution = stored.execution;
            let requested = Instant::now();
            let load = self
                .runtime
                .spawn(async move { store.output(execution, (page * PAGE) as i64).await });
            cx.spawn(async move |this, cx| {
                let result = load.await;
                let _ = this.update(cx, |this, cx| {
                    let Some(stored) = &mut this.stored else {
                        return;
                    };
                    stored.loading.remove(&page);
                    match result {
                        Ok(Ok(rows)) => {
                            stored.load_times.push(requested.elapsed());
                            stored.pages.insert(page, rows);
                            stored.recency.push_back(page);
                            while stored.pages.len() > CACHED_PAGES {
                                let Some(oldest) = stored.recency.pop_front() else {
                                    break;
                                };
                                if !stored.recency.contains(&oldest) {
                                    stored.pages.remove(&oldest);
                                }
                            }
                            stored.max_cached_pages =
                                stored.max_cached_pages.max(stored.pages.len());
                            cx.notify();
                        }
                        Ok(Err(error)) => this.fail(format!("page load failed: {error}"), cx),
                        Err(error) => this.fail(format!("page task failed: {error}"), cx),
                    }
                });
            })
            .detach();
        }
        format!("{:>6}  loading…", index + 1)
    }
}

async fn seed(directory: PathBuf) -> Result<(Store, i64, f64), nightshift::storage::Error> {
    let _ = std::fs::remove_dir_all(&directory);
    let started = Instant::now();
    let store = Store::open(directory).await?;
    let project = store
        .add_project(NewProject {
            checkout_path: "/nightshift-perf/fixture".into(),
            github_host: "github.com".into(),
            repository_id: "R_perf".into(),
            owner: "perf".into(),
            name: "fixture".into(),
            remote: "origin".into(),
            setup_command: None,
        })
        .await?;
    let execution = Coordinator::new(store.clone(), NonZeroU32::MIN)
        .reserve(NewExecution {
            project_id: project.id,
            issue: IssueSnapshot {
                id: "I_perf".into(),
                number: 1,
                title: "Performance fixture".into(),
                description: "Synthetic stored output".into(),
            },
            parent: None,
        })
        .await?;
    store
        .change(execution.id, ExecutionChange::Started, "Fixture".into())
        .await?;
    let mut written = 0;
    while written < STORED_BLOCKS {
        let batch = (written..(written + 4_000).min(STORED_BLOCKS))
            .map(|index| {
                (
                    if index % 7 == 0 { "stderr" } else { "agent" }.to_string(),
                    format!(
                        "stored block {index:06} · synthetic agent output for paging measurement {}",
                        "x".repeat(index % 48)
                    ),
                )
            })
            .collect::<Vec<_>>();
        written += batch.len();
        store.append_output_batch(execution.id, batch).await?;
    }
    Ok((store, execution.id, perf::elapsed_ms(started)))
}

impl Render for PerfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        if self.first_render_ms.is_none() {
            self.first_render_ms = Some(perf::elapsed_ms(self.config.main_started));
            let main_started = self.config.main_started;
            let entity = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = entity.update(cx, |this, _| {
                    this.first_frame_ms = Some(perf::elapsed_ms(main_started));
                    this.first_frame_unix_ms = Some(perf::unix_ms(SystemTime::now()));
                });
            });
        }
        if self.scenario_started.is_some() && !self.finished {
            if let Some(last) = self.last_render {
                self.frames.push(now - last);
            }
            self.last_render = Some(now);
            if let Some(input) = self.pending_input.take() {
                self.input_latency.push(now - input);
            }
            self.frame_index += 1;
            let frame = self.frame_index;
            self.issue_scroll
                .scroll_to_item(frame % replay::ISSUE_COUNT, gpui::ScrollStrategy::Top);
            // Steady scrolling plus a far jump every two seconds to force uncached page loads.
            let far = (frame / 120).wrapping_mul(7_919);
            self.stored_scroll
                .scroll_to_item((frame * 5 + far) % STORED_BLOCKS, gpui::ScrollStrategy::Top);
            window.request_animation_frame();
        }
        if self.config.baseline {
            return div()
                .size_full()
                .bg(rgb(0x18212c))
                .text_color(rgb(0xe3eaf1))
                .p_3()
                .child(format!(
                    "Baseline · {} · frames {}",
                    self.status,
                    self.frames.len()
                ))
                .into_any_element();
        }
        let selected = self.selected_issue;
        let replay_count = self.cache.blocks.len();
        let stored_count = if self.stored.is_some() {
            STORED_BLOCKS
        } else {
            0
        };
        let column = |title: String| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .border_r_1()
                .border_color(rgb(0x354352))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(rgb(0x9badbf))
                        .child(title),
                )
        };
        let element = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x18212c))
            .text_color(rgb(0xe3eaf1))
            .text_sm()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        column(format!("ISSUES · {}", replay::ISSUE_COUNT)).child(
                            uniform_list(
                                "perf-issues",
                                replay::ISSUE_COUNT,
                                cx.processor(move |_, range: std::ops::Range<usize>, _, _| {
                                    range
                                        .map(|index| {
                                            div()
                                                .id(("perf-issue", index))
                                                .h(px(ISSUE_ROW))
                                                .px_3()
                                                .flex()
                                                .flex_col()
                                                .justify_center()
                                                .border_l_2()
                                                .border_color(rgb(if index == selected {
                                                    0x9ec5f8
                                                } else {
                                                    0x18212c
                                                }))
                                                .child(format!(
                                                    "#{} Synthetic issue title {index}",
                                                    index + 1
                                                ))
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(rgb(0x9badbf))
                                                        .child("owner/repo · ready-for-agent"),
                                                )
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&self.issue_scroll)
                            .flex_1(),
                        ),
                    )
                    .child(
                        column(format!("REPLAY · {} received", self.cache.received)).child(
                            uniform_list(
                                "perf-replay",
                                replay_count,
                                cx.processor(move |this, range: std::ops::Range<usize>, _, _| {
                                    range
                                        .map(|index| {
                                            let block = this.cache.blocks[index];
                                            div()
                                                .h(px(OUTPUT_ROW))
                                                .px_3()
                                                .truncate()
                                                .font_family("Menlo")
                                                .text_xs()
                                                .child(format!(
                                                    "{} {:>7} {}",
                                                    block.stream,
                                                    block.sequence,
                                                    String::from_utf8_lossy(&block.payload[..48])
                                                ))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&self.replay_scroll)
                            .flex_1(),
                        ),
                    )
                    .child(
                        column(format!("STORED OUTPUT · {stored_count}")).child(
                            uniform_list(
                                "perf-stored",
                                stored_count,
                                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                    range
                                        .map(|index| {
                                            div()
                                                .h(px(OUTPUT_ROW))
                                                .px_3()
                                                .truncate()
                                                .font_family("Menlo")
                                                .text_xs()
                                                .child(this.stored_row(index, cx))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&self.stored_scroll)
                            .flex_1(),
                        ),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(0x354352))
                    .text_xs()
                    .text_color(rgb(0x9badbf))
                    .child(format!(
                        "Performance scenario · {} · frames {}",
                        self.status,
                        self.frames.len()
                    )),
            );
        if self.scenario_started.is_some() && !self.finished {
            self.view_build.push(now.elapsed());
        }
        element.into_any_element()
    }
}

pub fn run(runtime: Handle, config: PerfConfig) {
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("dev.nightshift.app".into()),
                titlebar: Some(TitlebarOptions {
                    title: Some("Nightshift performance scenario".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| PerfView::new(config, runtime, cx)),
        )
        .expect("open performance window");
        cx.activate(true);
    });
}
