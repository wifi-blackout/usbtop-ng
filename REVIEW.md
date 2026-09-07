# Review policy

Every change gets these passes, in severity order, and no change merges until a
human approves it. An agent never approves its own change.

## Passes

1. **Correctness / logic** — the change does what its spec and plan say; edge
   cases and error paths hold.
2. **Security** — kernel FFI and syscall use; TOCTOU, symlink, and privilege
   hazards (the support-bundle write path is the standing precedent); redaction
   and the SEC-1 (payload-free traces) and SEC-2 (sysfs containment)
   invariants; any leak of host or user identity.
3. **Spec-compliance** — the diff implements the originating
   `docs/superpowers/specs/…` spec and nothing outside its scope.
4. **Test coverage** — every change carries its proof. Tests assert specific
   behavior or values and would fail against the defect they guard; a bug fix
   has a failing test written and committed first, then the fix, without editing
   the test. "Untestable" must be shown, not asserted.
5. **Repo gates** — clippy `-D warnings` on all four configs, `cargo fmt`, MSRV
   1.88, zero `#[allow]`/`#[expect]`, no dead code, and no `Bin` (NUL-corrupted)
   source files.

## Severity

- **Critical** — correctness or security; blocks merge.
- **Important** — blocks merge; fix before proceeding.
- **Minor** — recorded for later.

Findings are reported most-severe first.

## Codex reviews: when, and which variant

A Codex review is a separate gate from the per-task subagent review. The
subagent reviewer gates each task inside a session; a Codex review looks at the
assembled work with an independent model.

| SDLC point | Review | Variant | When it applies |
|---|---|---|---|
| Design (spec committed, before the plan) | design challenge | adversarial, focused on the spec | architectural specs only; skip bounded or mechanical ones |
| Build (each task) | task review | subagent reviewer (not Codex) | every task; escalate one task to Codex standard only if it is unusually risky |
| Deploy (whole branch, pre-merge) | code review | adversarial | new feature or subsystem, security-sensitive, architectural, unsafe/FFI, or privacy/redaction touching |
| Deploy (whole branch, pre-merge) | code review | standard | bug fix, mechanical change, docs/config, or a small settled diff |
| Deploy (after fixes) | scoped re-review | standard | confirm the review's findings are resolved before merge |

The rule in one line: adversarial when the *approach* could be wrong, standard
when the approach is settled and only defects matter. Run large diffs in the
background and tiny ones in the foreground. The whole-branch review before a
feature merge is mandatory.

Invoke with `/codex:review` (standard) or `/codex:adversarial-review`
(adversarial), scoped with `--base main` for a branch.
