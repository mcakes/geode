//! Geode's background collector: a headless process that keeps the store
//! current while no app has it open, and hands it over when one appears.
//!
//! The binary (`geode-collector`) parses its arguments here and dispatches
//! to [`run`] (the collector loop, `run.rs`) or [`status`] (`status.rs`).
//! The current contracts are in `docs/current/data-path.md`.

pub mod install;
mod run;
mod status;

use std::path::{Path, PathBuf};

pub use run::{
    APP_POLL, EXIT_FAILED, ExeStamp, HOLD_POLL, STAMP_RECHECK, exe_changed, run, run_with_levels,
};
pub use status::status;

/// What the binary was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// The collector loop: what launchd or Task Scheduler starts.
    Run,
    /// Register for login and start now; `dry_run` prints the job document.
    Install {
        dry_run: bool,
    },
    Uninstall {
        dry_run: bool,
    },
    /// Who holds the store, from the lock files.
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Args {
    pub command: Command,
    /// `--demo [ROWS]`: the demo store with this many generated rows.
    pub demo_rows: Option<usize>,
}

/// The demo row count `--demo` takes without a number, as in the app.
const DEFAULT_DEMO_ROWS: usize = 100_000;

/// `[run|install|uninstall|status] [--dry-run] [--demo [ROWS]]`. The
/// command comes first and defaults to `run`; `--dry-run` belongs to
/// `install` and `uninstall` only. `--demo` takes the app's grammar: an
/// optional row count that must parse as `usize` (zero is accepted), else
/// 100,000. Anything else is an error carrying the usage text.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut rest = args;
    let mut command = Command::Run;
    if let Some((first, tail)) = rest.split_first() {
        let named = match first.as_str() {
            "run" => Some(Command::Run),
            "install" => Some(Command::Install { dry_run: false }),
            "uninstall" => Some(Command::Uninstall { dry_run: false }),
            "status" => Some(Command::Status),
            _ => None,
        };
        if let Some(named) = named {
            command = named;
            rest = tail;
        }
    }
    let mut demo_rows = None;
    let mut dry_run = false;
    while let Some((flag, tail)) = rest.split_first() {
        rest = tail;
        match flag.as_str() {
            "--demo" if demo_rows.is_none() => {
                let rows = match rest.split_first() {
                    Some((value, tail)) if !value.starts_with("--") => {
                        rest = tail;
                        value
                            .parse::<usize>()
                            .map_err(|_| usage(&format!("'{value}' is not a row count")))?
                    }
                    _ => DEFAULT_DEMO_ROWS,
                };
                demo_rows = Some(rows);
            }
            "--dry-run"
                if !dry_run
                    && matches!(command, Command::Install { .. } | Command::Uninstall { .. }) =>
            {
                dry_run = true;
            }
            _ => return Err(usage(&format!("unrecognised argument '{flag}'"))),
        }
    }
    let command = match command {
        Command::Install { .. } => Command::Install { dry_run },
        Command::Uninstall { .. } => Command::Uninstall { dry_run },
        other => other,
    };
    Ok(Args { command, demo_rows })
}

fn usage(reason: &str) -> String {
    format!(
        "{reason}\n\
         usage: geode-collector [run] [--demo [rows]]\n       \
         geode-collector install [--dry-run] [--demo [rows]]\n       \
         geode-collector uninstall [--dry-run] [--demo [rows]]\n       \
         geode-collector status [--demo [rows]]"
    )
}

/// The demo directory for `demo_rows`, as the app names it.
pub fn demo_root(demo_rows: Option<usize>) -> Option<PathBuf> {
    demo_rows.map(geode_compose::demo::demo_dir)
}

/// The login job for this executable and `demo_rows`: the absolute running
/// binary, and launchd's output files in `<user config>/logs` beside the
/// daily logs.
pub fn install_job(demo_rows: Option<usize>) -> Result<install::Job, String> {
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|err| format!("cannot locate this executable: {err}"))?;
    let (_, user) = geode_compose::config_dirs();
    let logs = user
        .ok_or("no user configuration directory (APPDATA or HOME is not set)")?
        .join("logs");
    Ok(install::job(&exe, demo_rows, &logs))
}

/// The store the app opens with the same arguments: configuration from the
/// data layer and the desk and user directories, then `store_path`.
pub fn store_for(demo_root: Option<&Path>) -> PathBuf {
    let config = geode_compose::load_config(demo_root, geode_compose::config_dirs());
    geode_compose::store_path(&config, demo_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        let owned: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        parse_args(&owned)
    }

    fn ok(command: Command, demo_rows: Option<usize>) -> Result<Args, String> {
        Ok(Args { command, demo_rows })
    }

    #[test]
    fn run_is_the_default_command() {
        assert_eq!(args(&[]), ok(Command::Run, None));
        assert_eq!(args(&["run"]), ok(Command::Run, None));
        assert_eq!(args(&["--demo"]), ok(Command::Run, Some(100_000)));
        assert_eq!(args(&["--demo", "1000"]), ok(Command::Run, Some(1000)));
        assert_eq!(args(&["run", "--demo", "0"]), ok(Command::Run, Some(0)));
    }

    #[test]
    fn install_takes_demo_rows_and_a_dry_run() {
        assert_eq!(
            args(&["install", "--demo", "1000"]),
            ok(Command::Install { dry_run: false }, Some(1000))
        );
        assert_eq!(
            args(&["install", "--dry-run", "--demo", "1000"]),
            ok(Command::Install { dry_run: true }, Some(1000))
        );
        // `--demo` before another flag takes the default row count.
        assert_eq!(
            args(&["uninstall", "--demo", "--dry-run"]),
            ok(Command::Uninstall { dry_run: true }, Some(100_000))
        );
        assert_eq!(
            args(&["uninstall"]),
            ok(Command::Uninstall { dry_run: false }, None)
        );
    }

    #[test]
    fn status_takes_demo_rows() {
        assert_eq!(args(&["status"]), ok(Command::Status, None));
        assert_eq!(
            args(&["status", "--demo", "7"]),
            ok(Command::Status, Some(7))
        );
    }

    #[test]
    fn an_unknown_or_misplaced_argument_is_an_error_with_usage() {
        for bad in [
            &["--nonesuch"][..],
            &["--demo", "abc"],
            &["status", "--dry-run"],
            &["run", "--dry-run"],
            &["--demo", "1", "--demo", "2"],
            &["install", "--dry-run", "--dry-run"],
            &["--demo", "1", "install"],
            &["stop"],
        ] {
            let err = args(bad).expect_err(&format!("{bad:?}"));
            assert!(err.contains("usage: geode-collector"), "{bad:?}: {err}");
        }
        assert!(
            args(&["--demo", "abc"])
                .unwrap_err()
                .contains("'abc' is not a row count")
        );
    }
}
