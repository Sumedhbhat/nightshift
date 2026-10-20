# Implementation decisions

Routine choices made during the autonomous MVP run, with the reason and tradeoff. Product scope remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md); the plan is in [implementation-plan.md](implementation-plan.md).

## D1. Validation channels

**Decision.** Use CI as the authoritative record for formatting, Clippy, tests, and release builds. Use the local scratch toolchain described in [overnight-progress.md](overnight-progress.md) for iteration and for running the Linux app.

**Reason.** The host has no C compiler, and the run may not install tools. CI runs on clean macOS and Linux runners, which also proves the build needs no sibling checkout.

**Tradeoff.** CI round trips take about ten minutes. Local results come from a nonstandard toolchain and are labelled as such.

## D2. Completion and follow-on work

**Decision.** Completing an execution never starts another search or execution by itself. Outside an active run, completion triggers nothing. During an owner-started run, a finish frees capacity, and the run's scheduler performs the refresh the MVP requires. Every candidate from that refresh passes the full start-eligibility check. Completion never causes publication of other executions.

**Reason.** Milestone 6 asks that completion trigger no extra search or execution, while [MVP_CONCEPT.md](../MVP_CONCEPT.md) requires a refresh when capacity frees during an active run. Treating the run, not completion, as the trigger satisfies both.

**Tradeoff.** Tests must cover both cases: no activity without a run, and gated refresh with one.
