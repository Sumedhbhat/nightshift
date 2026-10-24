# Desktop foundation

Nightshift should follow Zed's native Rust UI architecture and approach to responsiveness. Visual similarity is not a requirement.

The native foundation is complete. See [foundation-implementation.md](foundation-implementation.md) for its behavior, pinned dependencies, and performance measurements.

The MVP includes unattended progression through dependent issues and independent PR creation. Every execution starts from the latest fetched default branch and opens a PR against that branch. [Workflow orchestration](workflow-orchestration.md) describes how runs, executions, and publication are implemented.

## Architecture direction

- Use Rust and GPUI for the macOS and Linux application. [foundation-implementation.md](foundation-implementation.md) records the standalone build and responsiveness measurements.
- Use GPUI entities, declarative views, actions, and its platform event loop. Keep Git, GitHub, database access, and Codex process work outside the UI thread.
- Use Nightshift's [platform services](platform-services.md) for child-process environments and application data paths. macOS and Linux backends keep OS-specific shell defaults and tool locations out of GitHub and Codex adapters.
- Build Nightshift's own components and visual design. Refer to Zed's application code for patterns without adopting its editor workspace or entire crate graph.
- Measure startup, interaction latency, frame time, and memory under concurrent execution output. Using GPUI alone does not establish performance parity with Zed.

## Implementation references

- Use the sibling `../zed` checkout as the primary UI architecture reference.
- Follow the sibling `../t3code` checkout only for Codex integration. It launches `codex app-server`, communicates over standard input and output, and handles session events. Adapt this integration to Rust; use the existing Codex runtime.
- Relevant T3 Code files are `apps/server/src/provider/Layers/CodexSessionRuntime.ts`, `CodexAdapter.ts` in the same directory, and `packages/effect-codex-app-server/src/client.ts`.
- Relevant Zed references are `crates/gpui/README.md` and `crates/gpui_platform/`. Both sibling checkout paths above are relative to the Nightshift repository root.
- Storage uses SQLite with bounded output; see [storage-implementation.md](storage-implementation.md). Implementation decisions are recorded in [decisions.md](decisions.md).

Implement only the workflow in [MVP_CONCEPT.md](../MVP_CONCEPT.md).
