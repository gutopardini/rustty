//! Puts each shell in its own user scope when systemd is available.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);
const TIMEOUT: Duration = Duration::from_millis(900);

pub(crate) fn enabled() -> bool {
    std::env::var_os("RUSTTY_SYSTEMD_SCOPE").as_deref() != Some(std::ffi::OsStr::new("0"))
        && Path::new("/run/systemd/system").is_dir()
}

/// The child is still blocked before exec, with no descendants.
pub(crate) fn separate(pid: i32) -> io::Result<()> {
    let serial = NEXT_SCOPE.fetch_add(1, Ordering::Relaxed);
    let unit = format!("rustty-{}-{pid}-{serial}.scope", std::process::id());
    let pid_arg = pid.to_string();
    let deadline = Instant::now() + TIMEOUT;
    let mut request = Command::new("busctl")
        .args([
            "--user",
            "--timeout=800ms",
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            "ssa(sv)a(sa(sv))",
            &unit,
            "fail",
            "2",
            "Description",
            "s",
            "Rustty terminal session",
            "PIDs",
            "au",
            "1",
            &pid_arg,
            "0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    loop {
        match request.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(io::Error::other("systemd refused the tab scope"));
                }
                break;
            }
            Ok(None) => {}
            Err(err) => {
                let _ = request.kill();
                let _ = request.wait();
                return Err(err);
            }
        }
        if Instant::now() >= deadline {
            let _ = request.kill();
            let _ = request.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "systemd demorou ao criar o scope"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // The call returns when the job is queued; wait for the real migration.
    let cgroup_path = format!("/proc/{pid}/cgroup");
    loop {
        let cgroup = std::fs::read_to_string(&cgroup_path)?;
        if cgroup.lines().any(|line| line.rsplit('/').next() == Some(unit.as_str())) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "scope was not applied in time"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
