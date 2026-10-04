//! `geode-collector`: Geode's background collector. See the crate README.

use std::io::Write;
use std::time::Instant;

use geode_collector::{
    Args, Command, demo_root, install, install_job, parse_args, run_with_levels, status, store_for,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse_args(&args) {
        Err(message) => {
            to_stderr(&message);
            2
        }
        Ok(Args {
            command: Command::Run,
            demo_rows,
        }) => {
            // Only the loop logs: the other commands print, so a dry run or
            // a status probe creates no log file. Under launchd
            // (`GEODE_SERVICE` set) stderr is captured to an unpruned file,
            // so records go to the daily file only.
            let stderr = install::log_to_stderr(std::env::var_os(install::SERVICE_ENV));
            let logging = geode_compose::logging::install(&install::log_prefix(demo_rows), stderr);
            let code = run_with_levels(demo_rows, &Instant::now, Some(&*logging.control));
            // `process::exit` runs no destructors: drop the guard first so
            // the file writer flushes.
            drop(logging.guard);
            code
        }
        Ok(Args {
            command: Command::Status,
            demo_rows,
        }) => {
            println!("{}", status(&store_for(demo_root(demo_rows).as_deref())));
            0
        }
        Ok(Args {
            command: command @ (Command::Install { .. } | Command::Uninstall { .. }),
            demo_rows,
        }) => {
            let outcome = install_job(demo_rows).and_then(|job| {
                if matches!(command, Command::Install { .. })
                    && let Some(warning) = install::target_warning(&job.exe)
                {
                    to_stderr(&warning);
                }
                match command {
                    Command::Install { dry_run } => install::install(&job, dry_run),
                    Command::Uninstall { dry_run } => install::uninstall(&job, dry_run),
                    _ => unreachable!("matched install or uninstall"),
                }
            });
            match outcome {
                Ok(text) => {
                    println!("{}", text.trim_end());
                    0
                }
                Err(message) => {
                    to_stderr(&message);
                    1
                }
            }
        }
    };
    std::process::exit(code);
}

/// A command's message for the person at the terminal. Only `run` installs
/// logging (a dry run must write no log file), so usage errors, install
/// failures and the target-directory warning go to stderr directly.
fn to_stderr(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}
