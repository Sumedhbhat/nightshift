# Implementation references

- Build in Rust for macOS. Use the sibling `../zed` checkout as the primary UI reference; GPUI is a candidate, not a finalized dependency.
- Follow the sibling `../t3code` checkout only for Codex integration. It launches `codex app-server`, communicates over standard input and output, and handles session events. Adapt this integration to Rust; use the existing Codex runtime.
- Relevant T3 Code files are `apps/server/src/provider/Layers/CodexSessionRuntime.ts`, `CodexAdapter.ts` in the same directory, and `packages/effect-codex-app-server/src/client.ts`.
- Relevant Zed references are `crates/gpui/README.md` and `crates/gpui_platform/`. Both sibling checkout paths above are relative to the Nightshift repository root.
- The exact UI reuse, database engine, schema, and configuration format remain open for implementation planning.

Implement only the workflow in [MVP_CONCEPT.md](../MVP_CONCEPT.md).
