# geode-collector

Geode's background collector: a headless process that keeps the store
current while no app has it open, and hands it to the app when one appears.
It never depends on gpui or any UI crate; its configuration, store path,
logging and demo transports come from `geode-compose`, so it builds the same
store the app does.

Store ownership, the lease and the handoff:
[`docs/current/data-path.md`](../../docs/current/data-path.md).

## Commands

```text
geode-collector [run] [--demo [rows]]              the collector loop
geode-collector install [--dry-run] [--demo [rows]]
geode-collector uninstall [--dry-run] [--demo [rows]]
geode-collector status [--demo [rows]]             who holds the store
```

`--demo` takes the app's grammar (default 100,000 rows) and names the same
demo store. A usage error exits 2; a failed `install` or `uninstall` exits
1 with the failing step. Only `run` installs logging; the other commands
print to stdout and stderr, so a dry run or a status probe writes no file.

Exit statuses of `run`: 0 when another collector has the store (it stays
down), 75 (`EXIT_RESTART`, EX_TEMPFAIL) when the executable changed, so the
service manager starts the new build, and 70 (`EXIT_FAILED`) when a data
thread stopped with no app present, a release outlasted its 12 s watchdog,
or setup failed.

## Install

`install` registers the running binary (its absolute, canonical path) to
start at login and starts it now; `uninstall` reverses it. Install is the
only opt-in: the app never starts a collector. A successful install prints
the registered executable, and warns on stderr when it lies under a Cargo
`target` directory. `--dry-run` prints every file
the plan would create, write or remove, the document it would write, and
each command, and does none of it. On macOS even a dry run asks `id -u` for
the `gui/<uid>` domain it prints.

**macOS.** A LaunchAgent, label `com.geode.collector` (`.demo-<rows>` for a
demo store), in `~/Library/LaunchAgents/<label>.plist`: `ProgramArguments`
= the binary, `run`, and `--demo <rows>` for a demo store; `RunAtLoad`;
`KeepAlive = { SuccessfulExit = false }`, so launchd restarts a crash, an
exit 70 or an exit 75 (a changed binary), but not an exit 0 (a second
collector); `ThrottleInterval = 60`, so a persistent failure restarts once
a minute rather than every 10 s; `ProcessType = Background`;
`LowPriorityIO`; `EnvironmentVariables = { GEODE_SERVICE = 1 }`, which
makes the collector install logging without its stderr layer, plus
`GEODE_DESK_CONFIG` (made absolute) when it is set at install time.
launchd agents do not inherit the shell's environment, so a desk directory
set later, or only in a shell profile, never reaches the collector:
install again after changing it. The dry run shows the value written, and
the collector logs its desk and user directories at start, beside
`collecting into <db>`. launchd's stdout and stderr go to
`<prefix>-stdout.log` and `<prefix>-stderr.log` in the logs directory
(`collector`, or `collector-demo-<rows>` for a demo store; see
[Logging](#logging)); their names fall outside every daily
`<prefix>.*.log` trim, so they are never pruned. With `GEODE_SERVICE` set,
stderr holds only panics and failures from before logging started. The sequence: create the logs
directory, write the plist, `launchctl bootout gui/<uid>/<label>` (a job not
loaded, exit 3 or 113, is ignored; any other failure stops the install),
then `launchctl bootstrap gui/<uid> <plist>`. Uninstall boots the job out
and removes the plist.

**Windows.** A per-user Task Scheduler task `Geode\Collector`
(`Geode\Collector.demo-<rows>` for a demo store, the label's suffix): a
logon trigger and an interactive-token principal for `USERDOMAIN\USERNAME`,
`RestartOnFailure` every `PT1M` up to 3 times, `ExecutionTimeLimit` `PT0S`
(none). The XML is staged in the temp directory as UTF-16 with a BOM and
removed afterwards whatever the outcome. The sequence: `schtasks /End`
(any failure ignored), `schtasks /Create /TN <task> /XML <file> /F`, then
`schtasks /Run` so it starts now, as launchd's `RunAtLoad` does. Uninstall
ends the task and runs `schtasks /Delete /TN <task> /F`; a task that does
not exist ("cannot find") is not an error, so uninstalling twice succeeds
as on macOS. Task Scheduler captures no stderr, so the task sets no
`GEODE_SERVICE`. A logon task runs in the user's session with the user's
environment, so `GEODE_DESK_CONFIG` set for the user reaches it and the
task XML carries no environment.

Other platforms refuse with `install is supported on macOS and Windows`.
The plans are pure (`install_plan`, `uninstall_plan`) and run through a
`Runner`; tests use a recording runner and a temporary home, so no test
reaches `launchctl`, `schtasks` or the real `~/Library/LaunchAgents`.

## What lives here

| Module | Holds |
|---|---|
| `lib.rs` | `Command`, `Args`, `parse_args`; `demo_root` and `store_for`, the store the app opens with the same arguments; `install_job`, the login job for the running binary. |
| `install.rs` | `Job`, `job`, `task_name`; `log_prefix` and the launchd output file names; the document builders `launchd_plist` and `schtasks_xml` (with `xml_escape`, `utf16_with_bom`); `Platform`, `Host`, the `Runner` trait and `SystemRunner`; `Plan` with `install_plan`/`uninstall_plan`, `describe` (the dry run) and `execute`; `install`/`uninstall` and their `_with` forms. |
| `run.rs` | `run`/`run_with_levels`, the loop below; `ExeStamp`, `exe_changed` and `ExeWatch` (the executable check); the poll intervals and `RELEASE_WATCHDOG`; `finish_within` (the release watchdog); `BusyTimer` (a refused stamp read's escalation), `confirmed` (the two-probe app check) and `stop_report` (how a stopped hold is logged); the event sink (`Events`). |
| `status.rs` | `status(db)`: `collector: running\|not running; app: present\|absent; store: <db>`, from two lock probes. |
| `main.rs` | Installs logging with the store's prefix (`collector` or `collector-demo-<rows>`), dispatches, drops the log guard, exits with the returned status. |
| `tests/handoff.rs` | Cross-process tests: the built binary in a temp home against a temp store, this process (or a child of it) as the app. Also `measure_handoff`, the handoff measurement in `docs/perf.md` (ignored; `GEODE_MEASURE_HANDOFF=1`, release build). They run on Windows CI too but have been run only on macOS; the lost-race and momentary-lock tests are Unix only. |

## The loop

1. Take `<db>.collector.lock`, retried for 1 s (a dropping lease can be
   shared with a spawning child for an instant, and a `status` probe holds
   it for one). Still held: another collector has the store; log and exit 0.
2. Wait while an app holds `<db>.app.lock`, probing every second
   (`APP_POLL`). A probe error counts as an app present.
3. Read the store's stamp (`read_format`). A refused open (typically an app
   that appeared since the probe) goes back to step 2: it never exits. Once
   refusals with no app present have lasted `BUSY_LIMIT` (15 s, the app's
   own open deadline) the store is treated as unreadable. A stamp of another
   format logs one error naming both formats and idles, rechecking every
   30 s (`STAMP_RECHECK`); an unreadable stamp logs one error naming the
   store and the error and idles the same way.
4. Load the configuration afresh, apply `[log]` levels, and spawn the data
   service as `StoreRole::Collector` with `[collector] memory_limit` if
   set (unset by default: DuckDB's own limit, as the app), plus
   the demo bus in `--demo`.
5. Poll every 100 ms (`HOLD_POLL`) until an app appears or a data thread
   stops. An app counts only when a second probe 20 ms later sees it too:
   a `status` probe holds `<db>.app.lock` for an instant.
6. A stopped thread with an app present is the app winning the race for
   the store: log `app took the store first` at info and go back to step 2.
   With no app present it is an error, and the process exits 70.
7. Otherwise `release(HANDOFF_DRAIN)`, stop the bus, and log the release
   time. The release runs on its own thread under a 12 s watchdog
   (`RELEASE_WATCHDOG`, under the app's 15 s open deadline). A release
   still running then logs an error naming the store and the elapsed time
   and exits 70: the OS frees DuckDB's file lock and the lease locks,
   DuckDB rolls the unfinished transaction back, and the next owner
   rediscovers an unfinished file load. Otherwise go back to step 2.

Whenever the store is not held (at the top of each pass, every second of
the waits in steps 2 and 3, the idles, and after a release) the collector
compares its executable's size and mtime with those at start. A change
exits 75 (`EXIT_RESTART`) so the service manager starts the new build; a
collector idling on a mismatched stamp therefore restarts into a rebuilt
binary too. A missing executable (`cargo clean`, a removed worktree) is not
a change: it warns once and keeps running.

The collector spawns no child process: a fork shares the lease's open file
description, so the lock would outlive a drop until the child's exec.

## Logging

Everything goes to `tracing` under `geode::collector`, through the daily
`<user config>/logs/<prefix>.YYYY-MM-DD.log` file. The prefix is per store:
`collector` for the real store, `collector-demo-<rows>` for a demo store, so
a demo collector and the real one never share or trim each other's files
(the trim matches `<prefix>.`). The files are trimmed to the newest seven
at startup and again at each daily rotation. Health transitions per
source are info for ok and pending, warn for degraded, failed and pending
too long; publishes are debug; service diagnostics keep their
severity. The service's own open failure and every thread stop are not
logged by the sink: it holds them back, and the loop logs each once, at
info when an app explains it (step 6) and at error otherwise. Nothing in
the sink waits: it logs, updates a map or a list, and returns.

## Limits

- A store that refuses every read-only open (a corrupt or foreign file)
  keeps the collector idling at step 3 after one error, rather than
  exiting; it recovers on its own once the store reads.
- The open failure the sink holds back is recognised by the data service's
  `data service failed to open: ` prefix, which a test pins against a real
  failed open.
- A changed `data.db_path` takes effect only when the collector restarts:
  it holds the lease on the store it started with, and warns at each
  acquire while the configuration names another.
- The registered path is the binary that ran `install`. Installing from
  `target/debug` registers that build, and `cargo clean` or removing the
  checkout deletes it while the service manager keeps retrying; install
  prints a warning for a path under `target`.
- The release lets a file load under way finish, so a handoff during a
  large CSV load lasts the rest of that load (about 1.9 s at 1,000,000
  rows, 4.5 s at 2,000,000), past the 2 s drain. A release still running
  after 12 s (a longer load, an adapter call that never returns) exits 70:
  the app opens after about 12 s, the unfinished load is rolled back and
  rediscovered by the app, and the service manager restarts the collector
  (after up to 60 s on macOS). See `docs/current/performance.md`.
- `[collector] memory_limit` is unset by default. A finite limit of 512MB
  made DuckDB abort the collector with an internal assertion on large CSV
  loads, and a service manager would restart it into the same load; a
  value can be set once the overnight footprint measurement chooses one.
- A changed binary takes effect the next time the collector does not hold
  the store (exit 75). launchd restarts it after the plist's 60 s
  `ThrottleInterval`, so an upgrade restart can wait up to a minute. On
  Windows a running executable cannot be replaced, so a new build needs
  `install` again, which ends the task first.
- A reinstall's `launchctl bootout` or `schtasks /End` terminates a running
  collector without the release drain, so documents still pending in its
  coalescers can be lost. A logout's SIGTERM is the same: no drain.
  Recovery on subscribe restores each key's latest document in the next
  owner.
- Boot out or uninstall a demo collector
  (`geode-collector uninstall --demo <rows>`) before deleting
  `$TMPDIR/geode-demo/<rows>-42/`: a running one holds the store and its
  lease, and launchd restarts it into a fresh demo directory.
- App builds from worktrees cut before the app lease existed take no
  `<db>.app.lock`, so the collector never yields to them, and their open
  fails while it holds the store.
- On some macOS versions `launchctl bootstrap` right after `bootout` fails
  with `5: Input/output error` while the old job finishes exiting; run
  `install` again.
- Task Scheduler's `RestartOnFailure` restarts a task that fails to start;
  whether a nonzero exit code counts as a failure is Task Scheduler's
  decision and has not been checked on a Windows machine. Until it is, a
  Windows collector that exits 70 or 75 may stay down until the next
  logon. Registering the task is a run check there, not a CI test.
