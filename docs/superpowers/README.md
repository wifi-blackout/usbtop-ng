# Specs and plans

Every non-trivial change to usbtop-ng starts as a design spec in `specs/`
(`YYYY-MM-DD-<topic>-design.md`) and, once approved, an implementation plan
in `plans/` (`YYYY-MM-DD-<topic>.md`) that argues from it. The plan is
executed task by task with a review after each task, a whole-branch review
before the merge, and the Codex and Antigravity reviews `../../REVIEW.md`
prescribes.

The specs and plans written before 2026-09-07 were retired from the
repository at the checkpoint that removed every test-host name from the
tree; their decisions live on in `../ARCHITECTURE.md`, `../TESTING.md`,
`../ROADMAP.md`, and `../../CHANGELOG.md`. New specs and plans are committed
here as they are written, and describe hosts by the hardware-role labels
`../TESTING.md` uses, never by name.
