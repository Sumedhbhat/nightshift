# Native foundation implementation

This starts milestone 1 of the [implementation plan](implementation-plan.md). Product scope remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md). The synthetic replay does not establish execution completion, authorize runs, or implement publication.

The synthetic screen described here is historical. The application now opens the [project and public issue scaffold](public-issue-discovery.md); replay remains a library test utility.

## Build and structure

- One Rust 2024 package, pinned to Rust 1.97.1.
- `gpui` and `gpui_platform` use Zed revision `5a9b9558db01a6b906cec2fb70a797affdc58cdd`, fetched from GitHub. `Cargo.lock` pins the transitive dependencies.
- The platform enables `font-kit` for macOS text rendering, following the [upstream README at the pinned revision](https://github.com/zed-industries/zed/blob/5a9b9558db01a6b906cec2fb70a797affdc58cdd/crates/gpui/README.md).
- The first build found Xcode's optional Metal compiler missing. The platform's supported `runtime_shaders` feature compiles the embedded shaders through Metal at launch instead. Include this cost in startup measurements; revisit precompiled shaders for distribution.
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

The startup log measures entry to the first `Render` call. It is not a measurement of a presented frame or an interactive window. Full release-mode startup, frame-time, interaction-latency, and ten-minute memory measurements remain necessary before closing milestone 1.
