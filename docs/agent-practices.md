# Agent practices

Working habits for coding agents on Geode. `CLAUDE.md` holds the repository
rules (dependencies, UI, data, tests, commands); this guide does not repeat
them. It covers how work is run, verified and merged, and the hazards that
have cost real time. Each entry is a rule, the reason for it, and how to apply
it.

## Workflow and merging

**Size the loop to the change.** Non-trivial work (judgment across files, or a
blast radius not visible from one screen) runs: worktree, implementer,
independent reviewer, fix rounds until clean, merge, full workspace
verification, worktree and branch removal. A fix whose root cause you traced
yourself, in one file, with a few-line failing test, is done inline: test,
change, focused tests, merge. *Why:* the loop has caught real bugs at every
layer, but running it on a one-line fix wastes time and reads as defensive.

**Merging is pre-authorized.** When the review is clean and the workspace is
green, merge; the owner does the visual pass afterwards. Do not ask with a
menu of options.

**The owner edits main directly.** Run `git status` in the main checkout
before every merge. Uncommitted owner edits: verify the tree is green, commit
them as the owner's work with a plain description, then merge, resolving
conflicts to keep both sides. Never stash or overwrite them. Exception: dirty
files only minutes old while another agent session is live in the main
checkout are that session's work in progress. Do not commit or merge over
them; leave the branch merged up with main and green, and report the merge
command.

**The branch is the review record.** After a context reset, "merge the branch"
means the whole branch. Read its state (clean tree, review-round commits, docs
and harness entries, merged up with main) instead of relying on memory, and
never downgrade the merge to a cherry-pick.

**Main moves while you work.** A worktree is a snapshot of its base, not of
main's tip. Record the base SHA when the worktree is created and squash
against it (`git reset --soft <base>`), never against `main`: that turns every
newer main commit into an apparent revert. Before handing a branch to a
reviewer, check `git diff main --stat`; a file you never touched is the tell.
To recover a bad squash: `git diff <base> <bad> > patch`, `git reset --hard
main`, `git apply -3 patch`, verify, recommit. Before merging a long branch
that touches shared chrome or a cross-crate rule, check whether main already
moved on the same seam and adopt main's version where it supersedes yours.

**Resolve test and harness conflicts by hand.** Additive files (docs tables,
harness entries) usually keep both sides, but a blanket both-sides take on test
hunks or harness entries leaves stale counts and dangling fragments. When
merging main into a branch, compile main's new harness entries
(`--build-check "<name>"`): merges have brought in dead ones.

**Merging from an isolated worktree.** Git commands aimed at the main
checkout are refused from inside a worktree-isolated session. Exit the
worktree (keeping it), `git merge --ff-only <branch>` in the main checkout,
then `git worktree remove --force` and `git branch -d`. Keep git calls simple
and separate; compound shell commands are refused. Copy untracked review and
fix reports and `.superpowers/` ledgers to scratch before removing the
worktree. Remove merged worktrees promptly: each has its own large `target/`.

**Never let a pipe hide a failure.** In `a && b | tail && c`, a failed `a`
still lets `c` run (a failed ff merge once proceeded to `git worktree
remove`). Use `set -o pipefail`, or run the merge alone and check it before
anything destructive.

**`docs/superpowers/` is local only.** It is gitignored: write specs, plans and
review archives there, but never `git add -f` them or report them as
committed. After a merge, `git ls-files docs/superpowers` must be empty;
`git rm --cached` anything it lists. Worktrees do not contain these untracked
files, so pass absolute paths into the main checkout's copy. Behavior is
documented in `docs/current/` at merge.

**Verify in isolation.** The worktree's own full run, with its own `target/`,
is the verification of record. A red run in the main checkout right after a
merge can be another session building into the same target: compare `cargo
test -p <crate> -- --list` with the source's test functions, look for other
cargo processes, and rerun with `CARGO_TARGET_DIR=<scratch>/target` for a
race-free answer. Do not point worktrees at one shared target directory;
workspace-crate artifact hashes ignore the worktree path, so they overwrite
each other.

**Visual facts belong to the owner.** Agents have no display. Compensate with
real-event GPUI tests, and end every UI change with an honest list of display
checks still owed. For a visual or interaction choice, show options as
mockups (a Design artifact) before building; a bounded change can be approved
in conversation without a spec file.

## Reviews and verification

**Probe before belief.** A green suite and a thorough-looking test are not
evidence. Write a probe asserting the correct answer, run it against the
unmodified code, see it fail, then fix, and keep the probe as a permanent
test. Add one mutation entry per fix. *Why:* review rounds repeatedly found
defects the suite could not see; in one round five of eight deliberate
breakages left it green.

**Fixtures decide what a review can find.** Before reading code, build
fixtures for the shapes real configuration uses and existing tests do not;
a pair-grain defect was reachable only with the grain declared, as every real
schema does and no compiler test did. Report correctness evidence such as row counts alongside latency.

**Rule on mechanisms, not instances.** When a finding describes how a library
or subsystem behaves (for example, a table that re-reports its visible range
only when the numbers change), grep every site that relies on the opposite
before ruling, and price the ruling as "every site the mechanism reaches".
Pair the fix with a test at the level the mechanism bites: a painted cell, not
a cache lookup. *Why:* scoping such a fix to the noticed call site once
shipped a blank table to every other site.

**Split reviews of very large diffs.** A single reviewer stalls on a
whole-branch diff of hundreds of kilobytes; give parallel reviewers one slice
each.

## Subagents

**Use the default model.** Do not pass a cheaper model override to save quota.
If a usage limit is hit mid-task, say so and ask.

**Briefs carry the global constraints.** A task extract from a plan omits the
plan-level rules (dependency boundaries, no raw colors, render discipline,
pure cores first, how to handle drift from the plan). Put them in every
dispatch. After a mechanical edit to a plan or brief (a `sed` range delete),
check its line count; one such edit silently removed 300 lines.

**Long commands run in the foreground or from the controller.** Subagents that
background cargo and wait on it stall. Implementers run focused tests in the
foreground and commit each step; the controller runs the workspace test,
Clippy and long harness runs detached, with a bounded watcher. Checkpoint a
stalled agent's tree before resuming it, and resume the same agent after an
interruption rather than starting fresh.

**Interim "waiting" is not liveness.** Check `ps -axo
pid,ppid,etime,%cpu,command`. A `target/debug/deps/geode-*` test binary at 0%
CPU for minutes is blocked; at 100% for minutes it is looping (`sample <pid>`
shows where). A hung test binary looks like a slow build. Watchers must end
on process exit, not only on a success string. To stop a harness run: kill
the test binary, then `kill -TERM` the `mutation-check.sh` process (its trap
restores the mutated file; SIGKILL bypasses that), and confirm `git status`
is clean. Leave other sessions' runs alone. A run that died uncleanly can
leave `<git-dir>/mutation-check.lock`; `rmdir` it.

**Never wait with a self-matching `pgrep`.** `until ! pgrep -f
mutation-check.sh` matches the waiting shell's own command line and never
exits. Use the bracket form `pgrep -f 'mutation-che[c]k'`, or wait on a PID.

**Files over about 5,000 lines are never read whole.** Tell the agent to work
from `grep -n` outlines and `sed -n 'A,Bp'` ranges, move blocks with a short
script, and split the task by file group. Verify a pure move with a sorted,
whitespace-trimmed line multiset diff of before against the union of after;
only `mod`/`use`/`impl` wrappers, visibility prefixes and rustfmt reflows
should remain.

## Tests and mutation harness

The harness's own usage, verdicts and exit status are documented at the top
of `scripts/mutation-check.sh`; CLAUDE.md gives the commands.

**What a dispatched implementer may run.** `--anchors-only` and named entries
(a name substring), each in the foreground under `timeout 900`. Never
`--changed` and never a full run: `--changed` defaults to `main`, so on a
branch whose main has moved it selects other branches' entries and can hold
the per-checkout lock for hours. The controller runs `--changed=<task base>`
per task after merging main into the branch; selection is per file, so even a
comment edit in a heavily anchored file pulls in all its entries.

**An entry must prove its named test.** Every entry names its covering test.
Apply the mutation by hand once and confirm that test fails on an assertion,
not a compile error and not a different test.

**Pre-flight the anchors your change touches.** Ask both which anchors
contain lines you change and which text you add that an existing anchor
already matches; duplicating a line verbatim near an anchor makes it
ambiguous. Anchor multi-line blocks that include a line unique to the site.
When a new defence in front of an older one makes the older entry survive,
re-aim the entry at the site that now decides; never delete it.

**Tests a mutation targets must fail in bounded time.** No unbounded waits,
release held resources before asserting, and no assertion on elapsed time
(gate the work instead). The harness has no timeout, so a mutant that hangs
a test hangs the run.

**Test fixtures mirror production setup.** Initialize in `main.rs` order
(see `crates/geode-app/README.md`). A module test registry whose keymap
fragment binds a shell action must register
`geode_shell::defaults::register_builtin_actions` too, or every test that
builds that keymap breaks.

**Pointer tests reproduce the real host.** Host a tile under a focus-tracking
stand-in root, so a root focus grab that steals a mouse-opened field
reproduces. A test of a door the shell calls during its draw needs a stand-in
whose render reads the frame, or gpui's dropped-notify behavior cannot occur.
A double-click test sends two presses with a draw between them: a surface
closing above a table shifts it between the presses, which one press with a
click count of two hides.

## Editing hazards

**Mojibake in source.** A glyph painting as `â` plus accents (`â–¸` for `▸`)
is a source file re-encoded through Latin-1, not a font problem, and the suite
stays green because the tests' own literals are corrupted identically. Find
runs matching `[Â-ô][\x80-¿]{1,3}`, repair each with
`.encode('latin-1').decode('utf-8')`, and diff against the last clean
revision. Tests that pin glyphs spell them as `\u{25B8}`-style escapes.

**A doc rewrite is not docs-only.** Replacing a `//!` module header can delete
`mod` and `use` declarations interleaved with it; the resulting errors point
at other files. After any comment sweep over `.rs` files, run `cargo check
--workspace --all-targets` and list removed non-comment lines:

```sh
git diff -- '*.rs' | grep -E '^-' | grep -v '^---' | grep -vE '^-\s*(//|/\*|\*|$)'
```

## Conventions

**Spell it "color".** New code, comments, copy and config names say "color".
When a task edits a file containing `colour`, rename those identifiers in that
file and their direct callers in the same change, and re-anchor any harness
entries that name the changed lines. Old configuration spellings keep loading;
see `docs/current/configuration.md`.

**Check the component inventory before building UI.** The gpui-kit skill's
notes cover a subset of the library. Before writing any UI primitive, list
the cargo registry source of the exact `gpui-component` and `gpui-base`
versions pinned in the root `Cargo.toml` (`gpui-base` holds the unstyled
behavior: `Input`, the dock core, `focus_trap`, `motion`, `calendar`,
`test_support`), and the pinned `gpui-pre` for gpui itself. That source, not
a git checkout or the skill, is the API authority. Hand traces in reviews
read it too.

**Theme files need two checks nothing else performs.** Unknown keys in a
theme JSON deserialize silently to nothing, and `default.json` carries stale
spellings (`chart_1`, `link.foreground`, `window_border`, `drag_border`,
`progress_bar.background`, `slider.bar.background`; the live names are
`chart.1`, `link`, `window.border`, `drag.border`, `progress.bar.background`,
`slider.background`). For a new theme: parse it to `ThemeSet`, re-serialize
the colors, and assert every source key comes back non-null; then check WCAG
contrast over the information-bearing pairs. `tab.foreground` sits on
`tab_bar.background`, not `background`. Legibility beats palette fidelity
for data keys; fidelity wins for chrome. Record deliberate calls in the
commit message.

**Borrowed tokens need a distinctness sweep.** A state borrowed from a
gpui-component token (such as a hover fill) can equal its rest fill on some
bundled themes. Sweep every bundled theme for distinctness as well as
readability, as the shell's control and chip sweeps do.

## Product rulings that are easy to violate

**Confirm only what destroys.** Editing a builtin or desk object forks it to
the user layer at once and says so in a notice naming the undo verb; it never
asks. Delete and revert still confirm, because they destroy something. Do not
add a confirm whose only cost is a fork.

**A property is a verb, not a new entry point.** Naming an expression is done
on the expression (at creation, or later from its term), not through a
separate menu row, action or dialog for "named expressions". Extend existing
surfaces with the verb before adding a parallel one.

**Rulings stay ruled.** A choice the owner made on measured numbers (for
example, which dialogs prepare their rows, recorded in
`docs/current/performance.md`) is not re-proposed without new measurements,
and open items the owner explicitly closed are not raised again.
