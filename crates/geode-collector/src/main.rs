//! `geode-collector`: Geode's background collector. See the crate README.

use std::time::Instant;

use geode_collector::{
    Args, Command, demo_root, install, install_job, parse_args, run_with_levels, status, store_for,
};

fn main() {
    // Keep the guard until the explicit exit below: dropping it flushes and
    // stops the file writer, and `process::exit` runs no destructors.
    let logging = geode_compose::logging::install("collector");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse_args(&args) {
        Err(message) => {
            tracing::error!(target: "geode::collector", "{message}");
            2
        }
        Ok(Args {
            command: Command::Run,
            demo_rows,
        }) => run_with_levels(demo_rows, &Instant::now, Some(&*logging.control)),
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
            let outcome = install_job(demo_rows).and_then(|job| match command {
                Command::Install { dry_run } => install::install(&job, dry_run),
                Command::Uninstall { dry_run } => install::uninstall(&job, dry_run),
                _ => unreachable!("matched install or uninstall"),
            });
            match outcome {
                Ok(text) => {
                    println!("{}", text.trim_end());
                    0
                }
                Err(message) => {
                    tracing::error!(target: "geode::collector", "{message}");
                    1
                }
            }
        }
    };
    drop(logging.guard);
    std::process::exit(code);
}
