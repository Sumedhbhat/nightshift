# Desktop foundation

Nightshift should follow Zed's native Rust UI architecture and approach to responsiveness. Visual similarity is not a requirement. The implementation plan is in [implementation-plan.md](implementation-plan.md).

The first native foundation implementation is in progress. See [foundation-implementation.md](foundation-implementation.md) for its current behavior, pinned dependencies, and validation.

The MVP includes unattended progression through dependent issues and independent PR creation. Every execution starts from the latest fetched default branch and opens a PR against that branch. The implementation plan records the orchestration work still required.

## Architecture direction

- Use Rust and GPUI for the macOS application. This is the selected planning direction; the first implementation milestone must verify a standalone build and responsive rendering.
- Use GPUI entities, declarative views, actions, and its platform event loop. Keep Git, GitHub, database access, and Codex process work outside the UI thread.
- Build Nightshift's own components and visual design. Refer to Zed's application code for patterns without adopting its editor workspace or entire crate graph.
- Measure startup, interaction latency, frame time, and memory under concurrent execution output. Using GPUI alone does not establish performance parity with Zed.

## Implementation references

- Use the sibling `../zed` checkout as the primary UI architecture reference.
- Follow the sibling `../t3code` checkout only for Codex integration. It launches `codex app-server`, communicates over standard input and output, and handles session events. Adapt this integration to Rust; use the existing Codex runtime.
- Relevant T3 Code files are `apps/server/src/provider/Layers/CodexSessionRuntime.ts`, `CodexAdapter.ts` in the same directory, and `packages/effect-codex-app-server/src/client.ts`.
- Relevant Zed references are `crates/gpui/README.md` and `crates/gpui_platform/`. Both sibling checkout paths above are relative to the Nightshift repository root.
- The plan proposes SQLite, a small Rust module structure, and bounded output handling. Exact dependency versions and the remaining integration decisions must be resolved at the milestones listed there.

Implement only the workflow in [MVP_CONCEPT.md](../MVP_CONCEPT.md).
