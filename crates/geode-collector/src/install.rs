//! `install` and `uninstall`: register the collector to start at login, as a
//! macOS LaunchAgent or a Windows Task Scheduler logon task.
//!
//! Each operation is a [`Plan`] built by pure functions: the job document, the
//! files it writes or removes, and the commands it runs. `--dry-run` prints
//! the plan; a real run executes it through a [`Runner`], so tests drive the
//! executor with a recording runner and a temporary home and never reach
//! `launchctl` or `schtasks`.

use std::path::{Path, PathBuf};

/// The launchd label; a demo store adds `.demo-<rows>`.
pub const LABEL: &str = "com.geode.collector";
/// The Task Scheduler task name; a demo store adds the label's suffix.
pub const TASK: &str = "Geode\\Collector";
/// launchd's stdout and stderr files in the logs directory. Their names fall
/// outside the daily `collector.*.log` trim, which would otherwise delete
/// them as old daily logs.
pub const STDOUT_LOG: &str = "collector-stdout.log";
pub const STDERR_LOG: &str = "collector-stderr.log";

/// One registration: the collector's identity, command line and log files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// `com.geode.collector`, or `com.geode.collector.demo-<rows>`.
    pub label: String,
    /// The absolute executable the service manager starts.
    pub exe: PathBuf,
    /// `run`, then `--demo <rows>` for a demo store.
    pub args: Vec<String>,
    /// Where launchd writes stdout and stderr.
    pub log_dir: PathBuf,
}

/// The job for this executable and store. `exe` should be absolute: a
/// service manager has no working directory to resolve it against.
pub fn job(exe: &Path, demo_rows: Option<usize>, log_dir: &Path) -> Job {
    let mut args = vec!["run".to_string()];
    let mut label = LABEL.to_string();
    if let Some(rows) = demo_rows {
        args.extend(["--demo".to_string(), rows.to_string()]);
        label.push_str(&format!(".demo-{rows}"));
    }
    Job {
        label,
        exe: exe.to_path_buf(),
        args,
        log_dir: log_dir.to_path_buf(),
    }
}

/// The task name for `job`: `Geode\Collector`, plus the label's demo suffix
/// (`Geode\Collector.demo-<rows>`), so registering a demo store's task never
/// replaces the real store's.
pub fn task_name(job: &Job) -> String {
    let suffix = job.label.strip_prefix(LABEL).unwrap_or_default();
    format!("{TASK}{suffix}")
}

/// The LaunchAgent property list (spec §4): `RunAtLoad`, restart unless the
/// collector exited 0 (`KeepAlive.SuccessfulExit = false`: a second
/// collector and a changed binary exit 0; a crash or exit 70 restarts),
/// background scheduling and I/O, and stdout/stderr in the logs directory.
pub fn launchd_plist(job: &Job) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    key_string(&mut out, 1, "Label", &job.label);
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    string(&mut out, 2, &job.exe.to_string_lossy());
    for arg in &job.args {
        string(&mut out, 2, arg);
    }
    out.push_str("\t</array>\n");
    key_bool(&mut out, 1, "RunAtLoad", true);
    out.push_str("\t<key>KeepAlive</key>\n\t<dict>\n");
    key_bool(&mut out, 2, "SuccessfulExit", false);
    out.push_str("\t</dict>\n");
    key_string(&mut out, 1, "ProcessType", "Background");
    key_bool(&mut out, 1, "LowPriorityIO", true);
    let stdout = job.log_dir.join(STDOUT_LOG);
    let stderr = job.log_dir.join(STDERR_LOG);
    key_string(&mut out, 1, "StandardOutPath", &stdout.to_string_lossy());
    key_string(&mut out, 1, "StandardErrorPath", &stderr.to_string_lossy());
    out.push_str("</dict>\n</plist>\n");
    out
}

fn indent(out: &mut String, depth: usize) {
    out.extend(std::iter::repeat_n('\t', depth));
}

fn string(out: &mut String, depth: usize, value: &str) {
    indent(out, depth);
    out.push_str(&format!("<string>{}</string>\n", xml_escape(value)));
}

fn key_string(out: &mut String, depth: usize, name: &str, value: &str) {
    indent(out, depth);
    out.push_str(&format!("<key>{}</key>\n", xml_escape(name)));
    string(out, depth, value);
}

fn key_bool(out: &mut String, depth: usize, name: &str, value: bool) {
    indent(out, depth);
    out.push_str(&format!("<key>{}</key>\n", xml_escape(name)));
    indent(out, depth);
    out.push_str(if value { "<true/>\n" } else { "<false/>\n" });
}

/// The Task Scheduler task: a logon trigger for `user` (`DOMAIN\name`), run
/// as that user with an interactive token, restart on failure every minute
/// up to three times, and no time limit. The declaration names UTF-16, the
/// encoding `schtasks /XML` reads reliably; [`utf16_with_bom`] encodes it.
pub fn schtasks_xml(job: &Job, user: &str) -> String {
    let user = xml_escape(user);
    let uri = xml_escape(&format!("\\{}", task_name(job)));
    let command = xml_escape(&job.exe.to_string_lossy());
    let arguments = xml_escape(&command_line(&job.args));
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">
  <RegistrationInfo>
    <Description>Geode background collector: keeps the store current while no Geode window has it open.</Description>
    <URI>{uri}</URI>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id=\"Author\">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context=\"Author\">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"
    )
}

/// Arguments joined for a command line or a display: an empty argument, or
/// one with whitespace or a quote, is double-quoted with inner quotes
/// backslash-escaped.
fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
                arg.clone()
            } else {
                format!("\"{}\"", arg.replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` as UTF-16 little-endian with a byte-order mark.
pub fn utf16_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend(unit.to_le_bytes());
    }
    bytes
}

/// Escapes `&`, `<`, `>`, `"` and `'` for XML text and attributes.
pub fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// The service manager a plan targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// A per-user LaunchAgent (macOS).
    Launchd,
    /// A per-user Task Scheduler logon task (Windows).
    Schtasks,
}

impl Platform {
    /// This build's service manager; other platforms have none.
    pub fn current() -> Result<Platform, String> {
        if cfg!(target_os = "macos") {
            Ok(Platform::Launchd)
        } else if cfg!(windows) {
            Ok(Platform::Schtasks)
        } else {
            Err("install is supported on macOS and Windows".to_string())
        }
    }
}

/// What a plan needs from the machine, resolved once so the plan builders
/// stay pure and tests substitute a temporary home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    /// The home directory holding `Library/LaunchAgents` (macOS).
    pub home: PathBuf,
    /// The numeric user id for `gui/<uid>` (macOS).
    pub uid: String,
    /// `DOMAIN\name` for the logon trigger (Windows).
    pub user: String,
    /// Where the task XML is staged for `schtasks /XML` (Windows).
    pub temp_dir: PathBuf,
}

impl Host {
    /// The current user's: `HOME` and `id -u` for launchd; `USERDOMAIN`,
    /// `USERNAME` and the temp directory for Task Scheduler.
    pub fn current(platform: Platform, runner: &mut dyn Runner) -> Result<Host, String> {
        let mut host = Host {
            home: PathBuf::new(),
            uid: String::new(),
            user: String::new(),
            temp_dir: std::env::temp_dir(),
        };
        match platform {
            Platform::Launchd => {
                host.home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or("HOME is not set")?;
                let exit = runner.run("id", &["-u".to_string()])?;
                let uid = exit.stdout.trim();
                if exit.code != Some(0)
                    || uid.is_empty()
                    || !uid.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(format!(
                        "`id -u` did not give a user id: {}",
                        exit.stderr.trim()
                    ));
                }
                host.uid = uid.to_string();
            }
            Platform::Schtasks => {
                let name = std::env::var("USERNAME").map_err(|_| "USERNAME is not set")?;
                host.user = match std::env::var("USERDOMAIN") {
                    Ok(domain) if !domain.is_empty() => format!("{domain}\\{name}"),
                    _ => name,
                };
            }
        }
        Ok(host)
    }
}

/// A finished command: its exit code (`None` when killed by a signal) and
/// its output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a command to completion. The real one spawns the process; tests
/// record the calls instead, so no test reaches `launchctl` or `schtasks`.
pub trait Runner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<Exit, String>;
}

/// The process runner `install` and `uninstall` use.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<Exit, String> {
        let output = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| format!("{program}: {err}"))?;
        Ok(Exit {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Which failures of a step a plan continues past.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tolerate {
    /// Any failure stops the plan.
    Nothing,
    /// `launchctl bootout` of a service that is not loaded: exit 3 (no such
    /// process), 113 (could not find service), or those words on stderr.
    NotLoaded,
    /// Any exit, e.g. `schtasks /End` of a task that is missing or idle.
    Anything,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub program: String,
    pub args: Vec<String>,
    pub tolerate: Tolerate,
}

/// One step of a plan, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Create a directory and its parents.
    CreateDir(PathBuf),
    /// Write `text` to `path`, as UTF-16 with a BOM when `utf16`.
    Write {
        path: PathBuf,
        text: String,
        utf16: bool,
    },
    Run(Step),
    /// Remove a file; a missing file is not an error.
    Remove(PathBuf),
}

/// An install or uninstall: actions in order, files removed afterwards
/// whatever the outcome (the staged task XML), and the success message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub cleanup: Vec<PathBuf>,
    pub done: String,
}

/// `~/Library/LaunchAgents/<label>.plist` under `home`.
pub fn plist_path(job: &Job, home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", job.label))
}

fn step(program: &str, args: &[&str], tolerate: Tolerate) -> Action {
    Action::Run(Step {
        program: program.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        tolerate,
    })
}

/// Register `job` and start it now.
///
/// launchd: create the logs directory (launchd does not create it for
/// stdout/stderr), write the plist, `launchctl bootout gui/<uid>/<label>`
/// (a job not loaded is fine), then `launchctl bootstrap gui/<uid> <plist>`;
/// `RunAtLoad` starts it. Reinstalling replaces a running job.
///
/// Task Scheduler: stage the task XML as UTF-16 in the temp directory,
/// `schtasks /End` (any failure ignored: the task may not exist or not run),
/// `schtasks /Create /XML <file> /F`, then `schtasks /Run` so it starts now
/// as launchd's does rather than at the next logon. The staged file is
/// removed whatever the outcome.
pub fn install_plan(platform: Platform, job: &Job, host: &Host) -> Plan {
    match platform {
        Platform::Launchd => {
            let plist = plist_path(job, &host.home);
            let plist_arg = plist.to_string_lossy();
            let target = format!("gui/{}", host.uid);
            let service = format!("{target}/{}", job.label);
            Plan {
                actions: vec![
                    Action::CreateDir(job.log_dir.clone()),
                    Action::CreateDir(plist.parent().unwrap_or(&host.home).to_path_buf()),
                    Action::Write {
                        path: plist.clone(),
                        text: launchd_plist(job),
                        utf16: false,
                    },
                    step("launchctl", &["bootout", &service], Tolerate::NotLoaded),
                    step(
                        "launchctl",
                        &["bootstrap", &target, &plist_arg],
                        Tolerate::Nothing,
                    ),
                ],
                cleanup: Vec::new(),
                done: format!(
                    "installed {}: {} (started now and at each login)",
                    job.label,
                    plist.display()
                ),
            }
        }
        Platform::Schtasks => {
            let task = task_name(job);
            let staged = host
                .temp_dir
                .join(format!("{}-{}.xml", job.label, std::process::id()));
            let staged_arg = staged.to_string_lossy();
            Plan {
                actions: vec![
                    Action::CreateDir(host.temp_dir.clone()),
                    Action::Write {
                        path: staged.clone(),
                        text: schtasks_xml(job, &host.user),
                        utf16: true,
                    },
                    step("schtasks", &["/End", "/TN", &task], Tolerate::Anything),
                    step(
                        "schtasks",
                        &["/Create", "/TN", &task, "/XML", &staged_arg, "/F"],
                        Tolerate::Nothing,
                    ),
                    step("schtasks", &["/Run", "/TN", &task], Tolerate::Nothing),
                ],
                cleanup: vec![staged],
                done: format!("installed task {task} (started now and at each logon)"),
            }
        }
    }
}

/// Stop `job` and remove its registration: launchd boots the job out (one
/// not loaded is fine) and removes the plist (a missing one is fine); Task
/// Scheduler ends the task (any failure ignored) and deletes it, which
/// fails when no such task exists.
pub fn uninstall_plan(platform: Platform, job: &Job, host: &Host) -> Plan {
    match platform {
        Platform::Launchd => {
            let plist = plist_path(job, &host.home);
            let service = format!("gui/{}/{}", host.uid, job.label);
            Plan {
                actions: vec![
                    step("launchctl", &["bootout", &service], Tolerate::NotLoaded),
                    Action::Remove(plist.clone()),
                ],
                cleanup: Vec::new(),
                done: format!("uninstalled {}: removed {}", job.label, plist.display()),
            }
        }
        Platform::Schtasks => {
            let task = task_name(job);
            Plan {
                actions: vec![
                    step("schtasks", &["/End", "/TN", &task], Tolerate::Anything),
                    step(
                        "schtasks",
                        &["/Delete", "/TN", &task, "/F"],
                        Tolerate::Nothing,
                    ),
                ],
                cleanup: Vec::new(),
                done: format!("uninstalled task {task}"),
            }
        }
    }
}

/// `launchctl bootout`'s answers for a job that is not loaded.
fn not_loaded(exit: &Exit) -> bool {
    matches!(exit.code, Some(3) | Some(113))
        || ["No such process", "Could not find specified service"]
            .iter()
            .any(|words| exit.stderr.contains(words) || exit.stdout.contains(words))
}

fn display(step: &Step) -> String {
    let mut line = step.program.clone();
    if !step.args.is_empty() {
        line.push(' ');
        line.push_str(&command_line(&step.args));
    }
    line
}

impl Plan {
    /// The dry-run text: each file written (with its document), created or
    /// removed, and each command, in order.
    pub fn describe(&self) -> String {
        let mut out = String::from("dry run: nothing is written, removed or registered\n\n");
        for action in &self.actions {
            match action {
                Action::CreateDir(path) => {
                    out.push_str(&format!("create directory {}\n", path.display()));
                }
                Action::Write { path, text, utf16 } => {
                    let encoding = if *utf16 { " (UTF-16)" } else { "" };
                    out.push_str(&format!("write {}{encoding}:\n{text}", path.display()));
                    if !text.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Action::Run(step) => {
                    let note = match step.tolerate {
                        Tolerate::Nothing => "",
                        Tolerate::NotLoaded => "  (a job not loaded is ignored)",
                        Tolerate::Anything => "  (a failure is ignored)",
                    };
                    out.push_str(&format!("run: {}{note}\n", display(step)));
                }
                Action::Remove(path) => {
                    out.push_str(&format!("remove {}\n", path.display()));
                }
            }
        }
        for path in &self.cleanup {
            out.push_str(&format!(
                "remove {} (whatever the outcome)\n",
                path.display()
            ));
        }
        out
    }

    /// Runs the actions in order, stopping at the first failure not
    /// tolerated, then removes the cleanup files whatever the outcome.
    pub fn execute(&self, runner: &mut dyn Runner) -> Result<String, String> {
        let outcome = self
            .actions
            .iter()
            .try_for_each(|action| perform(action, runner))
            .map(|()| self.done.clone());
        for path in &self.cleanup {
            let _ = std::fs::remove_file(path);
        }
        outcome
    }
}

fn perform(action: &Action, runner: &mut dyn Runner) -> Result<(), String> {
    match action {
        Action::CreateDir(path) => std::fs::create_dir_all(path)
            .map_err(|err| format!("cannot create {}: {err}", path.display())),
        Action::Write { path, text, utf16 } => {
            let bytes = if *utf16 {
                utf16_with_bom(text)
            } else {
                text.as_bytes().to_vec()
            };
            std::fs::write(path, bytes)
                .map_err(|err| format!("cannot write {}: {err}", path.display()))
        }
        Action::Remove(path) => match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("cannot remove {}: {err}", path.display()))
            }
            _ => Ok(()),
        },
        Action::Run(step) => {
            let exit = runner.run(&step.program, &step.args)?;
            let tolerated = match step.tolerate {
                _ if exit.code == Some(0) => true,
                Tolerate::Nothing => false,
                Tolerate::NotLoaded => not_loaded(&exit),
                Tolerate::Anything => true,
            };
            if tolerated {
                return Ok(());
            }
            let code = exit
                .code
                .map_or_else(|| "killed by a signal".to_string(), |c| format!("exit {c}"));
            let detail = [exit.stderr.trim(), exit.stdout.trim()]
                .into_iter()
                .find(|s| !s.is_empty())
                .map(|s| format!(": {s}"))
                .unwrap_or_default();
            Err(format!("`{}` failed ({code}){detail}", display(step)))
        }
    }
}

/// `install` for this platform: the dry-run text, or the plan's outcome.
pub fn install(job: &Job, dry_run: bool) -> Result<String, String> {
    let platform = Platform::current()?;
    let mut runner = SystemRunner;
    let host = Host::current(platform, &mut runner)?;
    install_with(platform, job, dry_run, &host, &mut runner)
}

/// `uninstall` for this platform: the dry-run text, or the plan's outcome.
pub fn uninstall(job: &Job, dry_run: bool) -> Result<String, String> {
    let platform = Platform::current()?;
    let mut runner = SystemRunner;
    let host = Host::current(platform, &mut runner)?;
    uninstall_with(platform, job, dry_run, &host, &mut runner)
}

/// [`install`] with the platform, host and runner given. A dry run touches
/// no file and calls no runner.
pub fn install_with(
    platform: Platform,
    job: &Job,
    dry_run: bool,
    host: &Host,
    runner: &mut dyn Runner,
) -> Result<String, String> {
    act(install_plan(platform, job, host), dry_run, runner)
}

/// [`uninstall`] with the platform, host and runner given.
pub fn uninstall_with(
    platform: Platform,
    job: &Job,
    dry_run: bool,
    host: &Host,
    runner: &mut dyn Runner,
) -> Result<String, String> {
    act(uninstall_plan(platform, job, host), dry_run, runner)
}

fn act(plan: Plan, dry_run: bool, runner: &mut dyn Runner) -> Result<String, String> {
    if dry_run {
        Ok(plan.describe())
    } else {
        plan.execute(runner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers a recorded call (program first, then its arguments).
    type Reply = Box<dyn FnMut(&[String]) -> Exit>;

    /// Records every call and answers from `reply`; no process is spawned.
    struct Fake {
        calls: Vec<Vec<String>>,
        reply: Reply,
    }

    impl Fake {
        fn ok() -> Fake {
            Fake::with(|_| Exit {
                code: Some(0),
                ..Exit::default()
            })
        }

        fn with(reply: impl FnMut(&[String]) -> Exit + 'static) -> Fake {
            Fake {
                calls: Vec::new(),
                reply: Box::new(reply),
            }
        }
    }

    impl Runner for Fake {
        fn run(&mut self, program: &str, args: &[String]) -> Result<Exit, String> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().cloned());
            let exit = (self.reply)(&call);
            self.calls.push(call);
            Ok(exit)
        }
    }

    /// Fails the test on any call: a dry run runs nothing.
    struct Refuse;

    impl Runner for Refuse {
        fn run(&mut self, program: &str, args: &[String]) -> Result<Exit, String> {
            panic!("a dry run ran {program} {args:?}");
        }
    }

    fn host(root: &Path) -> Host {
        Host {
            home: root.join("home"),
            uid: "501".to_string(),
            user: "DESK\\me".to_string(),
            temp_dir: root.join("tmp"),
        }
    }

    fn temp_job(root: &Path) -> Job {
        job(
            Path::new("/opt/geode/geode-collector"),
            Some(1000),
            &root.join("logs"),
        )
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_launchd_dry_run_prints_the_plist_and_commands_and_touches_nothing() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let job = temp_job(root.path());
        let plist = plist_path(&job, &host.home);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "old").unwrap();
        let before = std::fs::metadata(&plist).unwrap().modified().unwrap();

        let text = install_with(Platform::Launchd, &job, true, &host, &mut Refuse).unwrap();
        assert!(text.contains(&launchd_plist(&job)), "{text}");
        assert!(
            text.contains(&format!("write {}", plist.display())),
            "{text}"
        );
        assert!(text.contains("launchctl bootout gui/501/com.geode.collector.demo-1000"));
        assert!(text.contains(&format!("launchctl bootstrap gui/501 {}", plist.display())));
        assert_eq!(std::fs::read_to_string(&plist).unwrap(), "old");
        assert_eq!(
            std::fs::metadata(&plist).unwrap().modified().unwrap(),
            before
        );
        assert!(!job.log_dir.exists());

        let text = uninstall_with(Platform::Launchd, &job, true, &host, &mut Refuse).unwrap();
        assert!(
            text.contains(&format!("remove {}", plist.display())),
            "{text}"
        );
        assert!(plist.exists());

        // A home with no LaunchAgents directory gains none.
        std::fs::remove_dir_all(host.home.join("Library")).unwrap();
        install_with(Platform::Launchd, &job, true, &host, &mut Refuse).unwrap();
        assert!(!host.home.join("Library").exists());
    }

    #[test]
    fn a_schtasks_dry_run_prints_the_task_and_commands_and_stages_nothing() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let job = temp_job(root.path());
        let text = install_with(Platform::Schtasks, &job, true, &host, &mut Refuse).unwrap();
        assert!(text.contains(&schtasks_xml(&job, "DESK\\me")), "{text}");
        assert!(
            text.contains("schtasks /End /TN Geode\\Collector.demo-1000"),
            "{text}"
        );
        assert!(text.contains("schtasks /Create /TN Geode\\Collector.demo-1000 /XML "));
        assert!(text.contains("schtasks /Run /TN Geode\\Collector.demo-1000"));
        assert!(!host.temp_dir.exists());
        let text = uninstall_with(Platform::Schtasks, &job, true, &host, &mut Refuse).unwrap();
        assert!(
            text.contains("schtasks /Delete /TN Geode\\Collector.demo-1000 /F"),
            "{text}"
        );
    }

    #[test]
    fn a_launchd_install_writes_the_plist_then_boots_out_and_bootstraps() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let job = temp_job(root.path());
        let plist = plist_path(&job, &host.home);
        // Not loaded yet: bootout's exit 113 is ignored.
        let mut fake = Fake::with(|call| Exit {
            code: Some(if call[1] == "bootout" { 113 } else { 0 }),
            ..Exit::default()
        });
        install_with(Platform::Launchd, &job, false, &host, &mut fake).unwrap();
        assert_eq!(
            std::fs::read_to_string(&plist).unwrap(),
            launchd_plist(&job)
        );
        assert!(job.log_dir.is_dir());
        let plist_arg = plist.to_string_lossy().into_owned();
        assert_eq!(
            fake.calls,
            [
                strings(&[
                    "launchctl",
                    "bootout",
                    "gui/501/com.geode.collector.demo-1000"
                ]),
                strings(&["launchctl", "bootstrap", "gui/501", &plist_arg]),
            ]
        );
    }

    #[test]
    fn a_failed_bootout_other_than_not_loaded_stops_the_install() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let job = temp_job(root.path());
        let mut fake = Fake::with(|_| Exit {
            code: Some(5),
            stderr: "Boot-out failed: 5: Input/output error".to_string(),
            ..Exit::default()
        });
        let err = install_with(Platform::Launchd, &job, false, &host, &mut fake).unwrap_err();
        assert!(
            err.contains("exit 5") && err.contains("Input/output error"),
            "{err}"
        );
        assert_eq!(fake.calls.len(), 1);
        // "not loaded" in words on any exit code is tolerated.
        let mut fake = Fake::with(|call| Exit {
            code: Some(if call[1] == "bootout" { 5 } else { 0 }),
            stderr: "Boot-out failed: 3: No such process".to_string(),
            ..Exit::default()
        });
        install_with(Platform::Launchd, &job, false, &host, &mut fake).unwrap();
        assert_eq!(fake.calls.len(), 2);
    }

    #[test]
    fn a_launchd_uninstall_boots_out_and_removes_the_plist() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let job = temp_job(root.path());
        let plist = plist_path(&job, &host.home);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "x").unwrap();
        let mut fake = Fake::ok();
        uninstall_with(Platform::Launchd, &job, false, &host, &mut fake).unwrap();
        assert!(!plist.exists());
        assert_eq!(
            fake.calls,
            [strings(&[
                "launchctl",
                "bootout",
                "gui/501/com.geode.collector.demo-1000"
            ])]
        );
        // Again, with nothing loaded and no file: still fine.
        let mut fake = Fake::with(|_| Exit {
            code: Some(3),
            ..Exit::default()
        });
        uninstall_with(Platform::Launchd, &job, false, &host, &mut fake).unwrap();
    }

    #[test]
    fn a_schtasks_install_stages_utf16_xml_and_removes_it_even_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let job = temp_job(root.path());
        let staged = std::rc::Rc::new(std::cell::RefCell::new(None::<Vec<u8>>));
        let seen = staged.clone();
        let mut fake = Fake::with(move |call| {
            if call[1] == "/Create" {
                let at = call.iter().position(|a| a == "/XML").unwrap() + 1;
                *seen.borrow_mut() = Some(std::fs::read(&call[at]).unwrap());
            }
            Exit {
                code: Some(if call[1] == "/End" { 1 } else { 0 }),
                ..Exit::default()
            }
        });
        install_with(Platform::Schtasks, &job, false, &host, &mut fake).unwrap();
        let names: Vec<&str> = fake.calls.iter().map(|c| c[1].as_str()).collect();
        assert_eq!(names, ["/End", "/Create", "/Run"]);
        assert_eq!(
            fake.calls[1][2..4],
            strings(&["/TN", "Geode\\Collector.demo-1000"])
        );
        assert_eq!(fake.calls[1].last().unwrap(), "/F");
        let bytes = staged.borrow().clone().unwrap();
        assert_eq!(bytes, utf16_with_bom(&schtasks_xml(&job, "DESK\\me")));
        assert_eq!(std::fs::read_dir(&host.temp_dir).unwrap().count(), 0);

        // A refused create fails the install and still removes the file.
        let mut fake = Fake::with(|call| Exit {
            code: Some(if call[1] == "/Create" { 1 } else { 0 }),
            stderr: "ERROR: Access is denied.".to_string(),
            ..Exit::default()
        });
        let err = install_with(Platform::Schtasks, &job, false, &host, &mut fake).unwrap_err();
        assert!(err.contains("Access is denied"), "{err}");
        assert_eq!(fake.calls.len(), 2);
        assert_eq!(std::fs::read_dir(&host.temp_dir).unwrap().count(), 0);
    }

    #[test]
    fn a_schtasks_uninstall_ends_then_deletes() {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path());
        let job = temp_job(root.path());
        let mut fake = Fake::ok();
        uninstall_with(Platform::Schtasks, &job, false, &host, &mut fake).unwrap();
        assert_eq!(
            fake.calls,
            [
                strings(&["schtasks", "/End", "/TN", "Geode\\Collector.demo-1000"]),
                strings(&[
                    "schtasks",
                    "/Delete",
                    "/TN",
                    "Geode\\Collector.demo-1000",
                    "/F"
                ]),
            ]
        );
    }

    #[test]
    fn utf16_has_a_bom_and_little_endian_units() {
        assert_eq!(utf16_with_bom("<é"), [0xFF, 0xFE, b'<', 0, 0xE9, 0]);
    }

    #[test]
    fn the_platform_is_this_builds_service_manager() {
        let platform = Platform::current();
        if cfg!(target_os = "macos") {
            assert_eq!(platform, Ok(Platform::Launchd));
        } else if cfg!(windows) {
            assert_eq!(platform, Ok(Platform::Schtasks));
        } else {
            assert_eq!(
                platform,
                Err("install is supported on macOS and Windows".to_string())
            );
        }
    }

    /// launchd's own output files sit beside the daily logs; the daily trim
    /// must not take them for old `collector.*.log` files.
    #[test]
    fn the_daily_trim_leaves_launchds_output_files() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=9 {
            std::fs::write(
                dir.path().join(format!("collector.2026-09-{day:02}.log")),
                "",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join(STDOUT_LOG), "out").unwrap();
        std::fs::write(dir.path().join(STDERR_LOG), "err").unwrap();
        geode_compose::logging::trim_log_files(dir.path(), "collector", 7);
        assert!(dir.path().join(STDOUT_LOG).exists());
        assert!(dir.path().join(STDERR_LOG).exists());
        let daily = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                let name = e.as_ref().unwrap().file_name();
                name.to_string_lossy().starts_with("collector.")
            })
            .count();
        assert_eq!(daily, 7);
    }

    /// The plist is a property list `plutil` accepts, with the spec's
    /// values. Read-only: `plutil -lint` and `-convert` on a temp file.
    #[cfg(target_os = "macos")]
    #[test]
    fn plutil_accepts_the_plist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job.plist");
        let job = job(
            Path::new("/opt/R&D/geode-collector"),
            Some(7),
            Path::new("/tmp/logs"),
        );
        std::fs::write(&path, launchd_plist(&job)).unwrap();
        let lint = std::process::Command::new("plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .unwrap();
        assert!(lint.status.success(), "{lint:?}");
        let json = std::process::Command::new("plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&path)
            .output()
            .unwrap();
        let json = String::from_utf8(json.stdout).unwrap();
        for part in [
            r#""KeepAlive":{"SuccessfulExit":false}"#,
            r#""RunAtLoad":true"#,
            r#""LowPriorityIO":true"#,
            r#""ProcessType":"Background""#,
            r#""ProgramArguments":["\/opt\/R&D\/geode-collector","run","--demo","7"]"#,
        ] {
            assert!(json.contains(part), "{part} in {json}");
        }
    }

    fn demo_job() -> Job {
        job(
            Path::new("/Applications/Geode/geode-collector"),
            Some(1000),
            Path::new("/Users/me/.config/geode/logs"),
        )
    }

    fn plain_job() -> Job {
        job(
            Path::new("/Applications/Geode/geode-collector"),
            None,
            Path::new("/Users/me/.config/geode/logs"),
        )
    }

    #[test]
    fn the_label_and_task_name_carry_the_demo_suffix() {
        assert_eq!(plain_job().label, "com.geode.collector");
        assert_eq!(demo_job().label, "com.geode.collector.demo-1000");
        assert_eq!(task_name(&plain_job()), "Geode\\Collector");
        assert_eq!(task_name(&demo_job()), "Geode\\Collector.demo-1000");
        assert_eq!(plain_job().args, ["run"]);
        assert_eq!(demo_job().args, ["run", "--demo", "1000"]);
    }

    #[test]
    fn the_plist_has_the_spec_keys() {
        let plist = launchd_plist(&demo_job());
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(plist.contains("<plist version=\"1.0\">"));
        assert!(
            plist
                .contains("\t<key>Label</key>\n\t<string>com.geode.collector.demo-1000</string>\n")
        );
        assert!(plist.contains(
            "\t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>/Applications/Geode/geode-collector</string>\n\
             \t\t<string>run</string>\n\
             \t\t<string>--demo</string>\n\
             \t\t<string>1000</string>\n\
             \t</array>\n"
        ));
        assert!(plist.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
        assert!(plist.contains(
            "\t<key>KeepAlive</key>\n\t<dict>\n\
             \t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\
             \t</dict>\n"
        ));
        assert!(plist.contains("\t<key>ProcessType</key>\n\t<string>Background</string>\n"));
        assert!(plist.contains("\t<key>LowPriorityIO</key>\n\t<true/>\n"));
        // Joined with the platform's separator, as the builder does.
        let logs = Path::new("/Users/me/.config/geode/logs");
        assert!(plist.contains(&format!(
            "\t<key>StandardOutPath</key>\n\t<string>{}</string>\n",
            logs.join("collector-stdout.log").display()
        )));
        assert!(plist.contains(&format!(
            "\t<key>StandardErrorPath</key>\n\t<string>{}</string>\n",
            logs.join("collector-stderr.log").display()
        )));
        assert!(plist.trim_end().ends_with("</dict>\n</plist>"));
    }

    #[test]
    fn the_plain_plist_runs_without_demo_arguments() {
        let plist = launchd_plist(&plain_job());
        assert!(plist.contains("<string>com.geode.collector</string>"));
        assert!(plist.contains("\t\t<string>run</string>\n\t</array>\n"));
        assert!(!plist.contains("--demo"));
    }

    #[test]
    fn xml_escape_covers_the_five_entities() {
        assert_eq!(
            xml_escape(r#"a&b<c>d"e'f"#),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        // `&` first: an entity is not escaped twice.
        assert_eq!(xml_escape("&lt;"), "&amp;lt;");
    }

    #[test]
    fn the_plist_and_task_escape_paths() {
        let job = job(
            Path::new("/Users/me/R&D <x>/geode-collector"),
            None,
            Path::new("/Users/me/R&D/logs"),
        );
        let plist = launchd_plist(&job);
        assert!(plist.contains("<string>/Users/me/R&amp;D &lt;x&gt;/geode-collector</string>"));
        let stdout = Path::new("/Users/me/R&amp;D/logs").join("collector-stdout.log");
        assert!(plist.contains(&format!("<string>{}</string>", stdout.display())));
        assert!(!plist.contains("R&D"));
        let task = schtasks_xml(&job, "DESK\\o'brien");
        assert!(task.contains("<Command>/Users/me/R&amp;D &lt;x&gt;/geode-collector</Command>"));
        assert!(task.contains("<UserId>DESK\\o&apos;brien</UserId>"));
        assert!(!task.contains("R&D"));
    }

    #[test]
    fn the_task_has_a_logon_trigger_restarts_and_no_time_limit() {
        let job = job(
            Path::new("C:\\Program Files\\Geode\\geode-collector.exe"),
            Some(1000),
            Path::new("C:\\Users\\me\\AppData\\Roaming\\geode\\logs"),
        );
        let task = schtasks_xml(&job, "DESK\\me");
        assert!(task.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n"));
        assert!(task.contains("<URI>\\Geode\\Collector.demo-1000</URI>"));
        assert!(task.contains(
            "    <LogonTrigger>\n      <Enabled>true</Enabled>\n      \
             <UserId>DESK\\me</UserId>\n    </LogonTrigger>\n"
        ));
        assert!(task.contains(
            "    <RestartOnFailure>\n      <Interval>PT1M</Interval>\n      \
             <Count>3</Count>\n    </RestartOnFailure>\n"
        ));
        assert!(task.contains("    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n"));
        assert!(task.contains(
            "      <Command>C:\\Program Files\\Geode\\geode-collector.exe</Command>\n      \
             <Arguments>run --demo 1000</Arguments>\n"
        ));
        assert!(task.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(task.trim_end().ends_with("</Task>"));
    }
}
