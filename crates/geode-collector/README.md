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
demo store. A usage error exits 2. `install` and `uninstall` are not built
yet and exit 2.

## What lives here

| Module | Holds |
|---|---|
| `lib.rs` | `Command`, `Args`, `parse_args`; `demo_root` and `store_for`, the store the app opens with the same arguments. |
| `run.rs` | `run`/`run_with_levels`, the loop below; `ExeStamp` and `exe_changed`; the poll intervals; `BusyTimer` (a refused stamp read's escalation), `confirmed` (the two-probe app check) and `stop_report` (how a stopped hold is logged); the event sink (`Events`). |
| `status.rs` | `status(db)`: `collector: running\|not running; app: present\|absent; store: <db>`, from two lock probes. |
| `main.rs` | Installs logging with the `collector` prefix, dispatches, drops the log guard, exits with the returned status. |

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
   service as `StoreRole::Collector` with `[collector] memory_limit`, plus
   the demo bus in `--demo`.
5. Poll every 100 ms (`HOLD_POLL`) until an app appears or a data thread
   stops. An app counts only when a second probe 20 ms later sees it too:
   a `status` probe holds `<db>.app.lock` for an instant.
6. A stopped thread with an app present is the app winning the race for
   the store: log `app took the store first` at info and go back to step 2.
   With no app present it is an error, and the process exits 70.
7. Otherwise `release(HANDOFF_DRAIN)`, stop the bus, and log the release
   time. If the executable's size or mtime changed since start, exit 0 so
   the service manager starts the new build; otherwise go back to step 2.

The collector spawns no child process: a fork shares the lease's open file
description, so the lock would outlive a drop until the child's exec.

## Logging

Everything goes to `tracing` under `geode::collector`, through the daily
`<user config>/logs/collector.YYYY-MM-DD.log` file. Health transitions per
source are info; publishes are debug; service diagnostics keep their
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
