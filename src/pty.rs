use std::ffi::CString;
use std::io::{self, ErrorKind};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::Mutex;

use nix::errno::Errno;
use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
use nix::pty::{forkpty, ForkptyResult, Winsize};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag};
use nix::unistd::{execve, write, Pid};

/// Lado mestre do PTY. O fork acontece antes de qualquer thread extra.
pub struct Pty {
    master: OwnedFd,
    child: Pid,
    write_lock: Mutex<()>,
}

impl Pty {
    pub fn spawn(cols: u16, rows: u16) -> io::Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let shell_c = CString::new(shell.clone()).map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))?;
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
        env.push(CString::new("TERM_PROGRAM=rustty").unwrap());

        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let forked = unsafe { forkpty(Some(&winsize), None::<&nix::sys::termios::Termios>) }
            .map_err(io_err)?;
        match forked {
            ForkptyResult::Child => {
                // O processo ainda tem uma única thread, então a alocação interna
                // do execve é segura. Qualquer falha encerra o filho.
                let args = [argv0];
                let _ = execve(&shell_c, &args, &env);
                unsafe { libc::_exit(127) };
            }
            ForkptyResult::Parent { child, master } => {
                let flags = OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL).map_err(io_err)?);
                fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).map_err(io_err)?;
                fcntl(&master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io_err)?;
                Ok(Self {
                    master,
                    child,
                    write_lock: Mutex::new(()),
                })
            }
        }
    }

    pub fn reader_fd(&self) -> io::Result<OwnedFd> {
        nix::unistd::dup(&self.master).map_err(io_err)
    }

    pub fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|err| err.into_inner());
        while !data.is_empty() {
            match write(&self.master, data) {
                Ok(0) => {
                    return Err(io::Error::new(ErrorKind::WriteZero, "pty fechado"));
                }
                Ok(n) => data = &data[n..],
                Err(Errno::EINTR) => continue,
                Err(Errno::EAGAIN) => {
                    let mut fds = [nix::poll::PollFd::new(
                        self.master.as_fd(),
                        nix::poll::PollFlags::POLLOUT,
                    )];
                    let _ = nix::poll::poll(&mut fds, 20u16);
                }
                Err(err) => return Err(io_err(err)),
            }
        }
        Ok(())
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
        let _ = waitpid(self.child, Some(WaitPidFlag::WNOHANG));
    }
}

fn io_err(err: Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}
