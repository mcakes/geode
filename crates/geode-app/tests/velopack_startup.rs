//! Exercise installer callbacks through the actual binary without installing
//! anything. These callbacks must exit before user state or windows exist.
#![cfg(windows)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn installer_hooks_exit_before_application_startup() {
    for hook in [
        "--veloapp-install",
        "--veloapp-updated",
        "--veloapp-obsolete",
        "--veloapp-uninstall",
    ] {
        let home = tempfile::tempdir().unwrap();
        let appdata = home.path().join("roaming");
        let local = home.path().join("local");
        let mut child = Command::new(env!("CARGO_BIN_EXE_geode"))
            .args([hook, "1.2.3"])
            .env("APPDATA", &appdata)
            .env("LOCALAPPDATA", &local)
            .env("HOME", home.path())
            .env_remove("GEODE_DESK_CONFIG")
            .env_remove("VELOPACK_DEBUG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("installer callback {hook} did not exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "installer callback {hook}: {status}");
        assert!(
            !appdata.exists(),
            "{hook} reached user configuration/logging"
        );
        assert!(!local.exists(), "{hook} reached the data store");
    }
}
