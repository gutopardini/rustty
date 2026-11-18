use std::collections::VecDeque;
use std::ffi::CString;
use std::io::{self, ErrorKind};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
use nix::pty::{forkpty, ForkptyResult, Winsize};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag};
use nix::unistd::{read, write, Pid};

/// Master side of the PTY and the session process.
pub struct Pty {
    master: OwnedFd,
    child: Pid,
    scope_setup: Option<JoinHandle<()>>,
    /// Bytes waiting for the reader thread. Only the tail that did not fit in the PTY.
    outgoing: Mutex<VecDeque<u8>>,
    write_lock: Mutex<()>,
    wake_read: OwnedFd,
    wake_write: OwnedFd,
    last_input: Mutex<Option<Instant>>,
}

impl Pty {
    pub fn spawn(cols: u16, rows: u16, cwd: Option<&Path>) -> io::Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let scope = crate::scope::enabled().then_some(crate::scope::separate as fn(i32) -> io::Result<()>);
        Self::spawn_shell(cols, rows, shell, scope, cwd)
    }

    fn spawn_shell(
        cols: u16,
        rows: u16,
        shell: String,
        scope: Option<fn(i32) -> io::Result<()>>,
        cwd: Option<&Path>,
    ) -> io::Result<Self> {
        let shell_c = CString::new(shell.clone()).map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))?;
        let cwd_c = cwd
            .map(|path| CString::new(path.as_os_str().as_bytes()))
            .transpose()
            .map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))?;
        let name = shell.rsplit('/').next().unwrap_or("bash");
        let argv0 = CString::new(format!("-{name}")).map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))?;
        let mut env = Vec::new();
        for (key, value) in std::env::vars() {
            if matches!(key.as_str(), "TERM" | "COLORTERM" | "TERM_PROGRAM") {
                continue;
            }
            if let Ok(pair) = CString::new(format!("{key}={value}")) {
                env.push(pair);
            }
        }
        env.push(CString::new("TERM=xterm-256color").unwrap());
        env.push(CString::new("COLORTERM=truecolor").unwrap());
        // Terminfo stays xterm. Codex enables /pets only when the program name
        // contains "kitty", "ghostty", or "wezterm", and it ignores TERM once
        // TERM_PROGRAM is set. The suffix advertises the graphics protocol.
        env.push(CString::new("TERM_PROGRAM=rustty-kitty").unwrap());
        // Build the vectors before fork: the child cannot allocate in a multithreaded process.
        let argv = [argv0.as_ptr(), std::ptr::null()];
        let mut envp: Vec<*const libc::c_char> = env.iter().map(|entry| entry.as_ptr()).collect();
        envp.push(std::ptr::null());

        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // The shell waits on the pipe until systemd moves its PID, so its
        // children are born in the right scope. Without systemd, it continues.
        let gate = if scope.is_some() {
            nix::unistd::pipe2(OFlag::O_CLOEXEC).ok()
        } else {
            None
        };
        let forked = unsafe { forkpty(Some(&winsize), None::<&nix::sys::termios::Termios>) }
            .map_err(io_err)?;
        match forked {
            ForkptyResult::Child => {
                if let Some((reader, writer)) = gate {
                    drop(writer);
                    let mut byte = 0u8;
                    loop {
                        let rc = unsafe { libc::read(reader.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
                        if rc >= 0 || Errno::last() != Errno::EINTR {
                            break;
                        }
                    }
                    drop(reader);
                }
                if let Some(dir) = &cwd_c {
                    unsafe {
                        libc::chdir(dir.as_ptr());
                    }
                }
                unsafe {
                    libc::execve(shell_c.as_ptr(), argv.as_ptr(), envp.as_ptr());
                    libc::_exit(127);
                }
            }
            ForkptyResult::Parent { child, master } => {
                let flags = OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL).map_err(io_err)?);
                fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).map_err(io_err)?;
                fcntl(&master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io_err)?;
                let (wake_read, wake_write) =
                    nix::unistd::pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).map_err(io_err)?;
                let scope_setup = gate.and_then(|(reader, writer)| {
                    drop(reader);
                    let setup = scope.expect("the pipe requires a scope setup");
                    std::thread::Builder::new().name("rustty-scope".into()).spawn(move || {
                        let result = setup(child.as_raw());
                        drop(writer);
                        if let Err(err) = result {
                            tracing::debug!(%err, "shell started without a separate scope");
                        }
                    }).ok()
                });
                Ok(Self {
                    master,
                    child,
                    scope_setup,
                    outgoing: Mutex::new(VecDeque::new()),
                    write_lock: Mutex::new(()),
                    wake_read,
                    wake_write,
                    last_input: Mutex::new(None),
                })
            }
        }
    }

    pub fn reader_fd(&self) -> io::Result<OwnedFd> {
        nix::unistd::dup(&self.master).map_err(io_err)
    }

    pub fn foreground_group(&self) -> Option<i32> {
        nix::unistd::tcgetpgrp(&self.master).ok().map(|group| group.as_raw()).filter(|group| *group > 0)
    }

    pub fn shell_pid(&self) -> i32 {
        self.child.as_raw()
    }

    pub fn note_input(&self) {
        *self.last_input.lock().unwrap_or_else(|err| err.into_inner()) = Some(Instant::now());
    }

    /// True for a short window after a key, while pet frames should stay on the previous picture.
    pub(crate) fn typing(&self) -> bool {
        self.last_input
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_some_and(|at| at.elapsed() < Duration::from_millis(120))
    }

    /// Write immediately. A full input buffer only queues the remainder; the event loop does not wait.
    pub fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let mut queued = false;
        {
            let _order = self.write_lock.lock().unwrap_or_else(|err| err.into_inner());
            let mut queue = self.outgoing.lock().unwrap_or_else(|err| err.into_inner());
            if !queue.is_empty() {
                queue.extend(data);
                queued = true;
            } else {
                while !data.is_empty() {
                    match write(&self.master, data) {
                        Ok(0) => return Err(io::Error::new(ErrorKind::WriteZero, "pty closed")),
                        Ok(n) => data = &data[n..],
                        Err(Errno::EINTR) => continue,
                        Err(Errno::EAGAIN) => {
                            queue.extend(data);
                            queued = true;
                            break;
                        }
                        Err(err) => return Err(io_err(err)),
                    }
                }
            }
        }
        if queued {
            match write(&self.wake_write, &[1]) {
                Ok(_) | Err(Errno::EAGAIN) | Err(Errno::EINTR) => {}
                Err(err) => return Err(io_err(err)),
            }
        }
        Ok(())
    }

    pub(crate) fn wake_fd(&self) -> BorrowedFd<'_> {
        self.wake_read.as_fd()
    }

    pub(crate) fn has_writes(&self) -> bool {
        !self.outgoing.lock().unwrap_or_else(|err| err.into_inner()).is_empty()
    }

    pub(crate) fn drain_wake(&self) {
        let mut buf = [0u8; 64];
        loop {
            match read(&self.wake_read, &mut buf) {
                Ok(0) | Err(Errno::EAGAIN) | Err(Errno::EINTR) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }

    pub(crate) fn flush_writes(&self) {
        let _order = self.write_lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut queue = self.outgoing.lock().unwrap_or_else(|err| err.into_inner());
        while !queue.is_empty() {
            let written = {
                let bytes = queue.make_contiguous();
                write(&self.master, bytes)
            };
            match written {
                Ok(0) => break,
                Ok(n) => {
                    queue.drain(..n);
                }
                Err(Errno::EINTR) => continue,
                Err(Errno::EAGAIN) => break,
                Err(_) => {
                    queue.clear();
                    return;
                }
            }
        }
    }

    pub fn hangup(&self) {
        let _ = kill(self.child, Signal::SIGHUP);
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(self.master.as_fd().as_raw_fd(), libc::TIOCSWINSZ, &winsize) };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl AsFd for Pty {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.master.as_fd()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = kill(self.child, Signal::SIGHUP);
        // Do not reuse the PID while the scope request still references it.
        if let Some(setup) = self.scope_setup.take() {
            let _ = setup.join();
        }
        let _ = waitpid(self.child, Some(WaitPidFlag::WNOHANG));
    }
}

fn io_err(err: Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        pty: Option<Pty>,
        dir: PathBuf,
        child: i32,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.pty.take());
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(scope: Option<fn(i32) -> io::Result<()>>) -> Fixture {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rustty-scope-test-{}-{serial}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let script = dir.join("shell.sh");
        std::fs::write(&script, b"#!/bin/sh\n/bin/sleep 30 &\nkid=$!\ntrap 'kill \"$kid\" 2>/dev/null; wait \"$kid\" 2>/dev/null' HUP TERM EXIT\nprintf '%s %s\\n' \"$kid\" \"$TERM_PROGRAM\"\nwait \"$kid\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pty = Pty::spawn_shell(80, 24, script.to_string_lossy().into_owned(), scope, None).unwrap();
        let reader = pty.reader_fd().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        while !output.contains(&b'\n') && Instant::now() < deadline {
            let mut fds = [nix::poll::PollFd::new(reader.as_fd(), nix::poll::PollFlags::POLLIN)];
            let _ = nix::poll::poll(&mut fds, 100u16);
            let mut bytes = [0u8; 128];
            if let Ok(n) = nix::unistd::read(&reader, &mut bytes) {
                output.extend_from_slice(&bytes[..n]);
            }
        }
        let text = String::from_utf8(output).unwrap();
        let mut fields = text.split_whitespace();
        let child = fields.next().expect("missing test process PID").parse().expect("invalid PID");
        assert_eq!(fields.next(), Some("rustty-kitty"));
        Fixture { pty: Some(pty), dir, child }
    }

    fn cgroup(pid: i32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap()
    }

    #[test]
    fn shell_still_starts_if_scope_setup_fails() {
        fn fail(_: i32) -> io::Result<()> { Err(io::Error::other("systemd unavailable")) }
        let fixture = fixture(Some(fail));
        let shell = fixture.pty.as_ref().unwrap().shell_pid();
        assert_eq!(fixture.pty.as_ref().unwrap().foreground_group(), Some(shell));
        assert_eq!(cgroup(shell), cgroup(std::process::id() as i32));
        assert_eq!(cgroup(fixture.child), cgroup(shell));
    }

    #[test]
    fn shell_and_child_use_a_separate_user_scope_when_available() {
        if !crate::scope::enabled() {
            return;
        }
        let available = Command::new("busctl")
            .args(["--user", "--timeout=500ms", "list"])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().is_ok_and(|status| status.success());
        if !available {
            return;
        }
        let fixture = fixture(Some(crate::scope::separate));
        let shell = fixture.pty.as_ref().unwrap().shell_pid();
        assert_eq!(fixture.pty.as_ref().unwrap().foreground_group(), Some(shell));
        let shell_group = cgroup(shell);
        assert!(shell_group.contains("/rustty-"), "{shell_group}");
        assert_ne!(shell_group, cgroup(std::process::id() as i32));
        assert_eq!(cgroup(fixture.child), shell_group);
    }

    #[test]
    fn shell_starts_in_the_configured_directory() {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("rustty-cwd-{}-{serial}", std::process::id()));
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let script = root.join("shell.sh");
        std::fs::write(&script, b"#!/bin/sh\npwd\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pty = Pty::spawn_shell(80, 24, script.to_string_lossy().into_owned(), None, Some(&work)).unwrap();
        let reader = pty.reader_fd().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        while !output.contains(&b'\n') && Instant::now() < deadline {
            let mut fds = [nix::poll::PollFd::new(reader.as_fd(), nix::poll::PollFlags::POLLIN)];
            let _ = nix::poll::poll(&mut fds, 100u16);
            let mut bytes = [0u8; 128];
            if let Ok(n) = nix::unistd::read(&reader, &mut bytes) {
                output.extend_from_slice(&bytes[..n]);
            }
        }
        drop(pty);
        let text = String::from_utf8(output).unwrap();
        let printed = PathBuf::from(text.lines().next().expect("missing directory"));
        assert_eq!(printed.canonicalize().unwrap(), work.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }
}
