//! Identifica o programa em primeiro plano de cada PTY, sem depender do título OSC.

use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;

pub const ICON_BASE: u32 = 0x11_0000;
pub const SETTINGS_ICON: u32 = ICON_BASE + 5;
pub const RUSTTY_ICON: u32 = ICON_BASE + 6;
pub const ICON_COUNT: u32 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Kiro,
    Antigravity,
    Cursor,
}

impl Agent {
    pub const fn icon(self) -> u32 {
        ICON_BASE + self as u32
    }
}

/// Inspeciona apenas o grupo de processos que tem o controle do terminal.
pub fn foreground(group: i32, shell: i32) -> Option<Agent> {
    if group <= 0 {
        return None;
    }
    foreground_in(Path::new("/proc"), group, shell)
}

fn foreground_in(proc_root: &Path, group: i32, shell: i32) -> Option<Agent> {
    let mut queue = VecDeque::from([group]);
    let mut visited = 0;
    while let Some(pid) = queue.pop_front() {
        visited += 1;
        if visited > 64 {
            break;
        }
        let path = proc_root.join(pid.to_string());
        if pid != group && process_group(&path) != Some(group) {
            continue;
        }
        if let Some(agent) = identify_process(&path) {
            return Some(agent);
        }
        let children = path.join("task").join(pid.to_string()).join("children");
        if let Ok(children) = std::fs::read_to_string(children) {
            queue.extend(children.split_whitespace().filter_map(|pid| pid.parse::<i32>().ok()));
        }
    }
    // Um agente pode entregar temporariamente o PTY a um subprocesso em outro
    // grupo. Só aceitamos ancestrais se a cadeia alcançar o shell desta aba.
    let mut current = group;
    let mut ancestor_agent = None;
    for _ in 0..32 {
        let path = proc_root.join(current.to_string());
        ancestor_agent = ancestor_agent.or_else(|| identify_process(&path));
        if current == shell {
            return ancestor_agent;
        }
        let Some(parent) = process_parent(&path) else { break };
        if parent <= 1 || parent == current {
            break;
        }
        current = parent;
    }
    None
}

fn identify_process(path: &Path) -> Option<Agent> {
    let mut command = Vec::new();
    if let Ok(file) = std::fs::File::open(path.join("cmdline")) {
        let _ = file.take(16 * 1024).read_to_end(&mut command);
    }
    let exe = std::fs::read_link(path.join("exe")).ok();
    identify(&command, exe.as_deref())
}

fn stat_field(path: &Path, field: usize) -> Option<i32> {
    let stat = std::fs::read_to_string(path.join("stat")).ok()?;
    // O nome entre parênteses pode conter espaços e até parênteses.
    stat.rsplit_once(')')?.1.split_whitespace().nth(field)?.parse().ok()
}

fn process_parent(path: &Path) -> Option<i32> {
    stat_field(path, 1)
}

fn process_group(path: &Path) -> Option<i32> {
    stat_field(path, 2)
}

fn identify(command: &[u8], exe: Option<&Path>) -> Option<Agent> {
    let args: Vec<&str> = command.split(|byte| *byte == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .filter(|part| !part.is_empty())
        .take(12)
        .collect();
    let executable = args.first().copied().unwrap_or("");
    let name = basename(executable);
    let direct = match name {
        "claude" | "claude-code" => Some(Agent::Claude),
        "codex" | "codex.js" | "codex-cli" => Some(Agent::Codex),
        "kiro" | "kiro-cli" | "kiro-cli.bin" | "kiro-cli-chat" => Some(Agent::Kiro),
        "agy" | "antigravity" | "antigravity-cli" => Some(Agent::Antigravity),
        "cursor" | "cursor-agent" | "cursor-cli" => Some(Agent::Cursor),
        _ => None,
    };
    if direct.is_some() {
        return direct;
    }
    let exe = exe.map(Path::to_string_lossy).unwrap_or_default();
    if exe.contains("/.local/share/claude/versions/") {
        return Some(Agent::Claude);
    }
    let cursor_binary = exe.contains("/cursor-agent/") || basename(&exe) == "cursor-agent";
    if name == "agent" && (cursor_binary || args.iter().skip(1).any(|arg| arg.contains("/cursor-agent/"))) {
        return Some(Agent::Cursor);
    }
    // Alguns instaladores usam Node ou shell para chamar o arquivo real.
    if matches!(name, "node" | "nodejs" | "bun" | "bash" | "sh") {
        for arg in args.iter().skip(1).take(5).filter(|arg| arg.contains('/')) {
            let lower = arg.to_ascii_lowercase();
            let filename = basename(&lower);
            if lower.contains("/@anthropic-ai/claude-code/") || filename == "claude-code" {
                return Some(Agent::Claude);
            }
            if lower.contains("/@openai/codex/") || matches!(filename, "codex.js" | "codex") {
                return Some(Agent::Codex);
            }
            if matches!(filename, "kiro-cli" | "kiro-cli.bin") {
                return Some(Agent::Kiro);
            }
            if matches!(filename, "agy" | "antigravity-cli") {
                return Some(Agent::Antigravity);
            }
            if lower.contains("/cursor-agent/") || matches!(filename, "cursor-agent" | "cursor") {
                return Some(Agent::Cursor);
            }
        }
    }
    None
}

fn basename(value: &str) -> &str {
    value.rsplit('/').next().unwrap_or(value).trim_start_matches('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_real_cli_names_and_wrappers_without_matching_prompt_text() {
        for (cmd, expected) in [
            (b"claude\0".as_slice(), Agent::Claude),
            (b"/usr/bin/node\0/opt/@openai/codex/bin/codex.js\0".as_slice(), Agent::Codex),
            (b"/home/me/.local/bin/kiro-cli.bin\0chat\0".as_slice(), Agent::Kiro),
            (b"agy\0".as_slice(), Agent::Antigravity),
            (b"/home/me/.local/bin/agent\0--use-system-ca\0/home/me/.local/share/cursor-agent/versions/v/index.js\0".as_slice(), Agent::Cursor),
        ] {
            assert_eq!(identify(cmd, None), Some(expected));
        }
        assert_eq!(identify(b"git\0commit\0-m\0use codex\0", None), None);
        assert_eq!(identify(b"agent\0", None), None);
    }

    #[test]
    fn reads_only_children_in_the_foreground_process_group() {
        let root = std::env::temp_dir().join(format!("rustty-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fn process(root: &Path, pid: i32, parent: i32, group: i32, cmd: &[u8], children: &str) {
            let path = root.join(pid.to_string());
            std::fs::create_dir_all(path.join("task").join(pid.to_string())).unwrap();
            std::fs::write(path.join("stat"), format!("{pid} (a process) S {parent} {group} 1 0 0")).unwrap();
            std::fs::write(path.join("cmdline"), cmd).unwrap();
            std::fs::write(path.join("task").join(pid.to_string()).join("children"), children).unwrap();
        }
        process(&root, 10, 1, 10, b"bash\0", "11 12");
        process(&root, 11, 10, 11, b"claude\0", "");
        process(&root, 12, 10, 10, b"codex\0", "");
        assert_eq!(foreground_in(&root, 10, 10), Some(Agent::Codex));
        process(&root, 15, 10, 15, b"claude\0", "20");
        process(&root, 20, 15, 20, b"bash\0", "");
        assert_eq!(foreground_in(&root, 20, 10), Some(Agent::Claude));
        assert_eq!(foreground_in(&root, 20, 99), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
