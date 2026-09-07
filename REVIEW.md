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

## Independent-model reviews: when, and which variant

Three distinct review engines look at every change that reaches the Deploy
row below, and none of them is the final word:

1. the **subagent reviewer** (a Claude subagent, per task, inside the session);
2. **Codex** (OpenAI), at the SDLC points in the table;
3. **Antigravity** (Gemini, via the `agy` CLI), run at every point where a
   Codex review runs, over the same scope and in the same variant
   (`--adversarial` when the Codex review is adversarial).

Codex and Antigravity are separate gates from the per-task subagent review:
the subagent reviewer gates each task inside a session; the two external
models look at the assembled work. Whenever a Codex review is performed, an
Antigravity review is performed too, so no change is assessed by fewer than
three engines. The session's Claude then reconciles every finding from all
three against the actual code, drops what does not hold, and is the **final
arbiter**: agreement across model families is a strong signal, disagreement is
a prompt to look closer, and a finding is neither accepted nor dismissed on
an engine's say-so alone.

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

Invoke Codex with `/codex:review` (standard) or `/codex:adversarial-review`
(adversarial), scoped with `--base main` for a branch, and Antigravity with
`/antigravity:review` (add `--adversarial` for the adversarial variant), scoped
to the same range (`main...HEAD` for a branch). The Antigravity diff is piped
by the session itself, never by a subagent (the delegate subagent's gate
refuses pipelines by design). Run the two in parallel, then reconcile.
