use std::io::Write;
use std::process::{Child, Command, Stdio};

/// Owns the clipboard selections. Each server stays alive until the next copy replaces it.
pub struct Clipboard {
    clipboard: Option<Child>,
    primary: Option<Child>,
    warned: bool,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            clipboard: None,
            primary: None,
            warned: false,
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        stop(&mut self.clipboard);
        stop(&mut self.primary);
    }
}

impl Clipboard {
    /// Puts `text` on the clipboard. `primary` also publishes the middle-click selection.
    pub fn set(&mut self, text: &str, primary: bool) {
        if !self.serve(text, false) && !self.warned {
            self.warned = true;
            tracing::warn!("clipboard needs wl-copy or xclip");
        }
        if primary {
            let _ = self.serve(text, true);
        }
    }

    pub fn get(&self, primary: bool) -> Option<String> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let args: &[_] = if primary {
                &["--no-newline", "--primary", "--type", "text/plain"]
            } else {
                &["--no-newline", "--type", "text/plain"]
            };
            if let Some(text) = command_output("wl-paste", args) {
                return Some(text);
            }
        }
        let selection = if primary { "primary" } else { "clipboard" };
        command_output("xclip", &["-selection", selection, "-o"])
    }

    fn serve(&mut self, text: &str, primary: bool) -> bool {
        let slot = if primary { &mut self.primary } else { &mut self.clipboard };
        stop(slot);
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let mut args = vec!["--foreground", "--type", "text/plain"];
            if primary {
                args.push("--primary");
            }
            if let Some(child) = spawn_server("wl-copy", &args, text) {
                *slot = Some(child);
                return true;
            }
        }
        let selection = if primary { "primary" } else { "clipboard" };
        if let Some(child) = spawn_server("xclip", &["-selection", selection, "-i"], text) {
            *slot = Some(child);
            return true;
        }
        false
    }
}

/// Bytes written to the shell. Newlines become carriage returns, and other controls are dropped.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(b'\r');
            }
            '\n' => out.push(b'\r'),
            '\t' => out.push(b'\t'),
            other if other.is_control() => {}
            other => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn spawn_server(program: &str, args: &[&str], text: &str) -> Option<Child> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    if stdin.write_all(text.as_bytes()).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    drop(stdin);
    match child.try_wait() {
        Ok(None) => Some(child),
        _ => None,
    }
}

fn stop(slot: &mut Option<Child>) {
    if let Some(mut child) = slot.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::paste_bytes;

    #[test]
    fn paste_normalizes_newlines_and_brackets_when_asked() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("a\nb", true), b"\x1b[200~a\rb\x1b[201~");
        assert_eq!(paste_bytes("a\u{1b}b\t", false), b"ab\t");
    }
}
