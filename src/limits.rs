//! Lightweight AI quota and usage addon: local O(1) readers, JSON drop-in, and floating popover.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::agent::Agent;
use crate::cell::{BOLD, Cell, DIM};
use crate::color::{Rgb, Theme};

/// Maximum cells the overlay layer allocates for a popover.
pub(crate) const MAX_CELLS: usize = 1024;
const MAX_VISIBLE_ENTRIES: usize = 6;
const TAIL_BYTES: u64 = 131_072;
const CMD_TIMEOUT: Duration = Duration::from_secs(4);
const CMD_COOLDOWN: Duration = Duration::from_secs(30);

/// Overall quota health derived from the lowest remaining percentage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Ok,
    Warn,
    Crit,
}

impl Health {
    pub fn from_percent(percent: u8) -> Self {
        if percent < 20 {
            Self::Crit
        } else if percent < 45 {
            Self::Warn
        } else {
            Self::Ok
        }
    }
}

/// Nested shorter-window meter (e.g. 5h window) paired under a parent quota bar (e.g. 7d).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubLimit {
    pub label: String,
    /// Remaining percentage, 0..=100.
    pub percent: u8,
    pub color: Rgb,
    pub hint: Option<String>,
}

/// One quota or context meter displayed inside the popover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub label: String,
    /// Remaining percentage, 0..=100.
    pub percent: u8,
    pub color: Rgb,
    /// Optional short hint such as `"2h15m"` or `"ctx"`.
    pub hint: Option<String>,
    /// Optional nested shorter window (e.g. 5h under 7d) rendered as a sleek sub-bar.
    pub sub: Option<SubLimit>,
    pub agent: Option<Agent>,
}

impl Entry {
    /// Lowest remaining percentage across the parent and any nested sub-window.
    pub fn effective_percent(&self) -> u8 {
        self.sub
            .as_ref()
            .map_or(self.percent, |sub| self.percent.min(sub.percent))
    }
}

/// Cached collection result rendered by the popover and reflected by the top-bar icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub title: String,
    pub subtitle: String,
    pub entries: Vec<Entry>,
    pub updated_at: Option<SystemTime>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            title: "AI Limits".to_string(),
            subtitle: "Quota · Models".to_string(),
            entries: Vec::new(),
            updated_at: None,
        }
    }
}

impl Snapshot {
    pub fn health(&self) -> Health {
        self.entries
            .iter()
            .map(Entry::effective_percent)
            .min()
            .map(Health::from_percent)
            .unwrap_or(Health::Ok)
    }
}

/// Cheap modification fingerprint so closed windows never re-parse unchanged files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint {
    stamps: [Option<(u64, u64)>; 5],
}

/// Clickable action inside the AI Limits popover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopoverHit {
    Refresh,
    Row(usize),
}

/// One GPU-drawn meter row inside the popover (in local cell coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterLayout {
    pub row: u16,
    pub dot_col: u16,
    pub bar_col: u16,
    pub bar_cols: u16,
    pub fill: f32,
    pub color: Rgb,
    /// Optional nested child bar `(fill, color)` drawn tightly under the main bar.
    pub sub: Option<(f32, Rgb)>,
}

/// Rendered cell overlay plus vector-shape geometry for the GPU pass.
#[derive(Clone, Debug)]
pub struct Popover {
    pub origin_col: u16,
    pub origin_row: u16,
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    /// Local cell rect `(col, row, cols, rows)` of the hero 3-layer icon.
    pub hero_icon: (u16, u16, u16, u16),
    /// Local vertical divider `(col, start_row, end_row)`, if horizontal layout is active.
    pub divider_v: Option<(u16, u16, u16)>,
    /// Local horizontal divider `(row, start_col, end_col)`, if compact vertical layout is active.
    pub divider_h: Option<(u16, u16, u16)>,
    pub meters: Vec<MeterLayout>,
    pub health: Health,
    refresh_zone: Option<(u16, u16, u16)>,
    row_zones: Vec<(u16, u16, u16, usize)>,
}

impl Popover {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.origin_col
            && row >= self.origin_row
            && col < self.origin_col.saturating_add(self.cols)
            && row < self.origin_row.saturating_add(self.rows)
    }

    pub fn hit(&self, col: u16, row: u16) -> Option<PopoverHit> {
        let local_col = col.checked_sub(self.origin_col)?;
        let local_row = row.checked_sub(self.origin_row)?;
        if local_col >= self.cols || local_row >= self.rows {
            return None;
        }
        if let Some((r, c0, c1)) = self.refresh_zone
            && local_row == r
            && (c0..c1).contains(&local_col)
        {
            return Some(PopoverHit::Refresh);
        }
        for &(r, c0, c1, idx) in &self.row_zones {
            if local_row == r && (c0..c1).contains(&local_col) {
                return Some(PopoverHit::Row(idx));
            }
        }
        None
    }
}

/// Curated vibrant colors matching the inspiration card.
const PALETTE: [Rgb; 6] = [
    Rgb::new(0x18, 0xD2, 0xFF), // Electric cyan
    Rgb::new(0x7C, 0x6C, 0xFF), // Indigo violet
    Rgb::new(0x1E, 0xE6, 0xA8), // Mint emerald
    Rgb::new(0xFB, 0x8C, 0x45), // Warm coral-orange
    Rgb::new(0x38, 0xBD, 0xF8), // Sky blue
    Rgb::new(0xF4, 0x72, 0xB6), // Rose pink
];

const WARN_COLOR: Rgb = Rgb::new(0xFB, 0x92, 0x3C);
const CRIT_COLOR: Rgb = Rgb::new(0xF8, 0x5A, 0x62);

/// Default path of the optional JSON drop-in file (`~/.config/rustty/limits.json`).
pub fn json_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("rustty").join("limits.json")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((secs, meta.len()))
}

/// Takes 5 fast `stat()` calls (~25 us) to detect whether local quota sources changed.
pub fn sources_fingerprint() -> Fingerprint {
    let custom = file_stamp(&json_path());
    let Some(home) = home_dir() else {
        return Fingerprint { stamps: [custom, None, None, None, None] };
    };
    let codex = latest_codex_file(&home).and_then(|p| file_stamp(&p));
    let claude = latest_claude_file(&home).and_then(|p| file_stamp(&p));
    let agy = latest_agy_file(&home).and_then(|p| file_stamp(&p));
    let cursor = latest_cursor_file(&home).and_then(|p| file_stamp(&p));
    Fingerprint { stamps: [custom, codex, claude, agy, cursor] }
}

/// Collects metrics from `~/.config/rustty/limits.json`, local AI CLI files, and an optional
/// user-configured command (rate-limited by `CMD_COOLDOWN` unless `force_cmd` is set).
pub fn collect(cmd: Option<&str>, allow_cmd: bool) -> Snapshot {
    let mut snap = Snapshot::default();
    let mut seen = HashSet::new();
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 1. Optional user-configured collector command (only when explicitly requested / allowed).
    if allow_cmd && let Some(command) = cmd.map(str::trim).filter(|c| !c.is_empty()) {
        if let Some(output) = run_collector_cmd(command) {
            apply_json(&output, now_unix, &mut snap, &mut seen);
        }
    }

    // 2. Drop-in addon file (~/.config/rustty/limits.json) takes priority over built-in heuristics.
    if let Ok(text) = std::fs::read_to_string(json_path()) {
        apply_json(&text, now_unix, &mut snap, &mut seen);
    }

    // 3. Built-in O(1) local readers (zero subprocesses, tail-only or stat-only reads).
    if let Some(home) = home_dir() {
        for entry in read_codex_entries(&home, now_unix) {
            push_unique(&mut snap.entries, &mut seen, entry);
        }
        if let Some(entry) = read_agy_entry(&home) {
            push_unique(&mut snap.entries, &mut seen, entry);
        }
        if let Some(entry) = read_claude_entry(&home) {
            push_unique(&mut snap.entries, &mut seen, entry);
        }
        if let Some(entry) = read_cursor_entry(&home) {
            push_unique(&mut snap.entries, &mut seen, entry);
        }
    }

    snap.entries.truncate(MAX_VISIBLE_ENTRIES);
    snap.updated_at = Some(SystemTime::now());
    snap
}

pub fn cmd_cooldown() -> Duration {
    CMD_COOLDOWN
}

fn push_unique(entries: &mut Vec<Entry>, seen: &mut HashSet<String>, entry: Entry) {
    let key = entry.id.to_ascii_lowercase();
    if seen.insert(key) {
        entries.push(entry);
    }
}

fn run_collector_cmd(command: &str) -> Option<String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + CMD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                if let Some(stdout) = child.stdout.take() {
                    let _ = stdout.take(65_536).read_to_string(&mut out);
                }
                return (!out.trim().is_empty()).then_some(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// Reads at most the last `TAIL_BYTES` bytes of `path`.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.take(TAIL_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Picks the newest entry in a directory using a key extractor.
fn newest_dir_entry(dir: &Path, want_dir: bool, ext: Option<&str>) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, String, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if want_dir != meta.is_dir() {
            continue;
        }
        if let Some(ext) = ext
            && path.extension().and_then(|e| e.to_str()) != Some(ext)
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
        let replace = match &best {
            None => true,
            Some((best_time, best_name, _)) => (mtime, &name) > (*best_time, best_name),
        };
        if replace {
            best = Some((mtime, name, path));
        }
    }
    best.map(|(_, _, path)| path)
}

/// Picks the highest lexicographically named numeric subdirectory (e.g. `2026` -> `11` -> `24`).
fn max_numeric_subdir(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let num: u32 = name.parse().ok()?;
            entry.file_type().ok()?.is_dir().then_some((num, entry.path()))
        })
        .max_by_key(|(num, _)| *num)
        .map(|(_, path)| path)
}

fn latest_codex_file(home: &Path) -> Option<PathBuf> {
    let sessions = home.join(".codex").join("sessions");
    let year = max_numeric_subdir(&sessions)?;
    let month = max_numeric_subdir(&year)?;
    let day = max_numeric_subdir(&month)?;
    newest_dir_entry(&day, false, Some("jsonl"))
}

fn read_codex_entries(home: &Path, now_unix: u64) -> Vec<Entry> {
    let Some(path) = latest_codex_file(home) else {
        return Vec::new();
    };
    let Some(tail) = read_tail(&path) else {
        return Vec::new();
    };
    parse_codex_rate_limits(&tail, now_unix)
}

/// Extracts `secondary` (7d parent) and `primary` (5h child) into a single unified Codex row.
pub(crate) fn parse_codex_rate_limits(text: &str, now_unix: u64) -> Vec<Entry> {
    let Some(pos) = text.rfind("\"rate_limits\":") else {
        return Vec::new();
    };
    let slice = &text[pos + "\"rate_limits\":".len()..];
    let Some(block) = extract_brace_object(slice) else {
        return Vec::new();
    };

    let parse_window = |key: &str, default_label: &str| -> Option<(String, u8, Option<String>)> {
        let needle = format!("\"{key}\":");
        let idx = block.find(&needle)?;
        let window_obj = extract_brace_object(&block[idx + needle.len()..])?;
        let used = json_f64(window_obj, "used_percent")?;
        let window_min = json_f64(window_obj, "window_minutes").map(|v| v.round() as u64);
        let resets_at = json_f64(window_obj, "resets_at").map(|v| v.round() as u64);
        let label = match window_min {
            Some(300) => "5h".to_string(),
            Some(10080) => "7d".to_string(),
            Some(m) if m >= 1440 => format!("{}d", m / 1440),
            Some(m) if m >= 60 => format!("{}h", m / 60),
            _ => default_label.to_string(),
        };
        let (percent, hint) = remaining_from_used(used, resets_at, now_unix);
        Some((label, percent, hint))
    };

    let primary = parse_window("primary", "5h");
    let secondary = parse_window("secondary", "7d");

    match (secondary, primary) {
        (Some((_, p7, h7)), Some((l5, p5, h5))) => vec![Entry {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            percent: p7,
            color: PALETTE[0],
            hint: h7,
            sub: Some(SubLimit {
                label: l5,
                percent: p5,
                color: Rgb::new(0x8B, 0x7C, 0xFF),
                hint: h5,
            }),
            agent: Some(Agent::Codex),
        }],
        (Some((_, p, h)), None) | (None, Some((_, p, h))) => vec![Entry {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            percent: p,
            color: PALETTE[0],
            hint: h,
            sub: None,
            agent: Some(Agent::Codex),
        }],
        (None, None) => Vec::new(),
    }
}

fn remaining_from_used(used: f64, resets_at: Option<u64>, now_unix: u64) -> (u8, Option<String>) {
    match resets_at {
        Some(ts) if ts > 0 && now_unix >= ts => (100u8, Some("reset".to_string())),
        Some(ts) if ts > now_unix => (
            (100.0 - used).round().clamp(0.0, 100.0) as u8,
            Some(format_countdown(ts - now_unix)),
        ),
        _ => ((100.0 - used).round().clamp(0.0, 100.0) as u8, None),
    }
}

fn latest_agy_file(home: &Path) -> Option<PathBuf> {
    let brain = home.join(".gemini").join("antigravity-cli").join("brain");
    let mut dirs: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(brain)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_dir().then(|| (meta.modified().unwrap_or(UNIX_EPOCH), entry.path()))
        })
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, dir) in dirs.into_iter().take(3) {
        let log = dir.join(".system_generated").join("logs").join("transcript.jsonl");
        if log.is_file() {
            return Some(log);
        }
    }
    None
}

fn read_agy_entry(home: &Path) -> Option<Entry> {
    let path = latest_agy_file(home)?;
    let tail = read_tail(&path)?;
    parse_agy_transcript(&tail)
}

pub(crate) fn parse_agy_transcript(text: &str) -> Option<Entry> {
    for line in text.lines().rev() {
        if !line.contains("\"input_tokens\"") || !line.contains("\"model\"") {
            continue;
        }
        let model = json_str(line, "model")?;
        let input = json_f64(line, "input_tokens").unwrap_or(0.0);
        let cache = json_f64(line, "cache_read_tokens").unwrap_or(0.0);
        let used = input + cache;
        let (id, label, color, window) = match model.to_ascii_lowercase().as_str() {
            m if m.contains("gemini") || m.contains("neon") || m.contains("carbon") || m.contains("argon") || m.contains("hydrogen") || m.contains("flash") => {
                ("gemini", "Gemini", PALETTE[2], 1_000_000.0)
            }
            m if m.contains("claude") => ("agy-claude", "Agy Claude", PALETTE[3], 200_000.0),
            _ => ("antigravity", "Antigravity", PALETTE[2], 1_000_000.0),
        };
        let window = if used > window { 1_000_000.0 } else { window };
        let remaining = ((1.0 - used / window) * 100.0).round().clamp(0.0, 100.0) as u8;
        return Some(Entry {
            id: id.to_string(),
            label: label.to_string(),
            percent: remaining,
            color,
            hint: Some("ctx".to_string()),
            sub: None,
            agent: Some(Agent::Antigravity),
        });
    }
    None
}

fn latest_claude_file(home: &Path) -> Option<PathBuf> {
    let projects = home.join(".claude").join("projects");
    let project = newest_dir_entry(&projects, true, None)?;
    newest_dir_entry(&project, false, Some("jsonl"))
}

fn read_claude_entry(home: &Path) -> Option<Entry> {
    let path = latest_claude_file(home)?;
    let tail = read_tail(&path)?;
    parse_claude_session(&tail)
}

fn latest_cursor_file(home: &Path) -> Option<PathBuf> {
    let projects = home.join(".cursor").join("projects");
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&projects).ok()?.flatten() {
        let transcripts = entry.path().join("agent-transcripts");
        let Ok(meta) = std::fs::metadata(&transcripts) else { continue };
        if !meta.is_dir() {
            continue;
        }
        let Some(session_dir) = newest_dir_entry(&transcripts, true, None) else {
            continue;
        };
        let Some(file) = newest_dir_entry(&session_dir, false, Some("jsonl")) else {
            continue;
        };
        let mtime = std::fs::metadata(&file)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(UNIX_EPOCH);
        if best.as_ref().is_none_or(|(best_t, _)| mtime > *best_t) {
            best = Some((mtime, file));
        }
    }
    best.map(|(_, path)| path)
}

fn read_cursor_entry(home: &Path) -> Option<Entry> {
    let path = latest_cursor_file(home)?;
    let bytes = std::fs::metadata(&path).ok()?.len();
    Some(cursor_entry_from_bytes(bytes))
}

pub(crate) fn cursor_entry_from_bytes(bytes: u64) -> Entry {
    // Cursor JSONL transcripts do not record exact server token counts locally;
    // estimate remaining context window from the transcript size (~4 bytes/token).
    let est_tokens = bytes as f64 / 4.0;
    let window = if est_tokens > 200_000.0 { 1_000_000.0 } else { 200_000.0 };
    let remaining = ((1.0 - est_tokens / window) * 100.0).round().clamp(0.0, 100.0) as u8;
    Entry {
        id: "cursor".to_string(),
        label: "Cursor".to_string(),
        percent: remaining,
        color: PALETTE[1],
        hint: Some("ctx".to_string()),
        sub: None,
        agent: Some(Agent::Cursor),
    }
}

pub(crate) fn parse_claude_session(text: &str) -> Option<Entry> {
    for line in text.lines().rev() {
        let Some(pos) = line.rfind("\"usage\":") else {
            continue;
        };
        let Some(usage) = extract_brace_object(&line[pos + "\"usage\":".len()..]) else {
            continue;
        };
        let input = json_f64(usage, "input_tokens").unwrap_or(0.0);
        let cache_create = json_f64(usage, "cache_creation_input_tokens").unwrap_or(0.0);
        let cache_read = json_f64(usage, "cache_read_input_tokens").unwrap_or(0.0);
        let used = input + cache_create + cache_read;
        if used <= 0.0 {
            continue;
        }
        let model = json_str(line, "model").unwrap_or_default();
        let window = if used > 200_000.0 || model.contains("5-") || model.contains("4-6") {
            1_000_000.0
        } else {
            200_000.0
        };
        let remaining = ((1.0 - used / window) * 100.0).round().clamp(0.0, 100.0) as u8;
        return Some(Entry {
            id: "claude".to_string(),
            label: "Claude".to_string(),
            percent: remaining,
            color: PALETTE[3],
            hint: Some("ctx".to_string()),
            sub: None,
            agent: Some(Agent::Claude),
        });
    }
    None
}

fn format_countdown(secs: u64) -> String {
    if secs < 60 {
        return "<1m".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 48 {
        let rem = mins % 60;
        if rem == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{rem:02}m")
        }
    } else {
        let days = hours / 24;
        let rem_h = hours % 24;
        if rem_h == 0 {
            format!("{days}d")
        } else {
            format!("{days}d{rem_h}h")
        }
    }
}

/// Parses either a JSON array of entry objects or `{ "title", "subtitle", "items": [...] }`.
pub(crate) fn apply_json(text: &str, now_unix: u64, snap: &mut Snapshot, seen: &mut HashSet<String>) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let array_slice = if trimmed.starts_with('[') {
        Some(trimmed)
    } else if trimmed.starts_with('{') {
        if let Some(title) = json_str(trimmed, "title")
            && !title.is_empty()
        {
            snap.title = title;
        }
        if let Some(subtitle) = json_str(trimmed, "subtitle")
            && !subtitle.is_empty()
        {
            snap.subtitle = subtitle;
        }
        ["items", "entries", "limits", "models"]
            .into_iter()
            .find_map(|key| {
                let needle = format!("\"{key}\"");
                let pos = trimmed.find(&needle)?;
                let after = trimmed[pos + needle.len()..].trim_start();
                let after = after.strip_prefix(':')?.trim_start();
                extract_bracket_array(after)
            })
    } else {
        None
    };

    let Some(array) = array_slice else { return };
    for (idx, obj) in iter_json_objects(array).into_iter().enumerate() {
        let Some(label) = json_str(obj, "label").or_else(|| json_str(obj, "name")) else {
            continue;
        };
        if label.trim().is_empty() {
            continue;
        }
        // Strip nested `"sub":{...}` before reading top-level numeric keys so they never collide.
        let sub_obj = find_json_value(obj, "sub").and_then(extract_brace_object);
        let top_obj = match sub_obj {
            Some(sub) => CowObj::Stripped(obj.replacen(sub, "{}", 1)),
            None => CowObj::Borrowed(obj),
        };
        let top = top_obj.as_str();
        let resets_at = json_f64(top, "resets_at").map(|v| v.round() as u64);
        let (percent, reset_hint) = if let Some(p) = json_f64(top, "percent").or_else(|| json_f64(top, "remaining")) {
            if resets_at.is_some_and(|ts| ts > 0 && now_unix >= ts) {
                (100u8, Some("reset".to_string()))
            } else {
                let hint = resets_at.filter(|&ts| ts > now_unix).map(|ts| format_countdown(ts - now_unix));
                (p.round().clamp(0.0, 100.0) as u8, hint)
            }
        } else if let Some(used) = json_f64(top, "used_percent") {
            remaining_from_used(used, resets_at, now_unix)
        } else {
            continue;
        };
        let id = json_str(top, "id").unwrap_or_else(|| label.to_ascii_lowercase());
        let color = json_str(top, "color")
            .and_then(|hex| parse_hex_color(&hex))
            .unwrap_or(PALETTE[(snap.entries.len() + idx) % PALETTE.len()]);
        let hint = json_str(top, "reset").or_else(|| json_str(top, "hint")).or(reset_hint);
        let agent = json_str(top, "agent").and_then(|name| parse_agent(&name));
        let sub = parse_sub_limit(sub_obj, top, color, now_unix);
        push_unique(
            &mut snap.entries,
            seen,
            Entry { id, label, percent, color, hint, sub, agent },
        );
    }
}

enum CowObj<'a> {
    Borrowed(&'a str),
    Stripped(String),
}

impl<'a> CowObj<'a> {
    fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(s) => s,
            Self::Stripped(s) => s.as_str(),
        }
    }
}

fn parse_sub_limit(sub_obj: Option<&str>, top: &str, parent_color: Rgb, now_unix: u64) -> Option<SubLimit> {
    let default_color = Rgb::new(
        parent_color.r.saturating_add(45),
        parent_color.g.saturating_add(45),
        parent_color.b.saturating_add(35),
    );
    if let Some(sub) = sub_obj {
        let resets_at = json_f64(sub, "resets_at").map(|v| v.round() as u64);
        let (percent, reset_hint) = if let Some(p) = json_f64(sub, "percent").or_else(|| json_f64(sub, "remaining")) {
            if resets_at.is_some_and(|ts| ts > 0 && now_unix >= ts) {
                (100u8, Some("reset".to_string()))
            } else {
                let hint = resets_at.filter(|&ts| ts > now_unix).map(|ts| format_countdown(ts - now_unix));
                (p.round().clamp(0.0, 100.0) as u8, hint)
            }
        } else if let Some(used) = json_f64(sub, "used_percent") {
            remaining_from_used(used, resets_at, now_unix)
        } else {
            return None;
        };
        let label = json_str(sub, "label").unwrap_or_else(|| "5h".to_string());
        let color = json_str(sub, "color")
            .and_then(|hex| parse_hex_color(&hex))
            .unwrap_or(default_color);
        let hint = json_str(sub, "reset").or_else(|| json_str(sub, "hint")).or(reset_hint);
        return Some(SubLimit { label, percent, color, hint });
    }
    let sub_resets = json_f64(top, "sub_resets_at").map(|v| v.round() as u64);
    let (percent, reset_hint) = if let Some(p) = json_f64(top, "sub_percent") {
        if sub_resets.is_some_and(|ts| ts > 0 && now_unix >= ts) {
            (100u8, Some("reset".to_string()))
        } else {
            let hint = sub_resets.filter(|&ts| ts > now_unix).map(|ts| format_countdown(ts - now_unix));
            (p.round().clamp(0.0, 100.0) as u8, hint)
        }
    } else if let Some(used) = json_f64(top, "sub_used_percent") {
        remaining_from_used(used, sub_resets, now_unix)
    } else {
        return None;
    };
    let label = json_str(top, "sub_label").unwrap_or_else(|| "5h".to_string());
    let color = json_str(top, "sub_color")
        .and_then(|hex| parse_hex_color(&hex))
        .unwrap_or(default_color);
    let hint = json_str(top, "sub_reset").or(reset_hint);
    Some(SubLimit { label, percent, color, hint })
}

fn parse_agent(name: &str) -> Option<Agent> {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => Some(Agent::Claude),
        "codex" | "openai" => Some(Agent::Codex),
        "kiro" => Some(Agent::Kiro),
        "antigravity" | "agy" | "gemini" => Some(Agent::Antigravity),
        "cursor" => Some(Agent::Cursor),
        _ => None,
    }
}

fn parse_hex_color(hex: &str) -> Option<Rgb> {
    let hex = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb::new(r, g, b))
}

fn extract_delimited(text: &str, open: u8, close: u8) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| !b.is_ascii_whitespace())?;
    if bytes[start] != open {
        return None;
    }
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_brace_object(text: &str) -> Option<&str> {
    extract_delimited(text, b'{', b'}')
}

fn extract_bracket_array(text: &str) -> Option<&str> {
    extract_delimited(text, b'[', b']')
}

fn iter_json_objects(array: &str) -> Vec<&str> {
    let mut objects = Vec::new();
    let bytes = array.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_str = true;
                i += 1;
            }
            b'{' => {
                if let Some(obj) = extract_brace_object(&array[i..]) {
                    objects.push(obj);
                    i += obj.len();
                } else {
                    break;
                }
            }
            _ => i += 1,
        }
    }
    objects
}

fn find_json_value<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let mut rest = obj;
    while let Some(pos) = rest.find(&needle) {
        let after = rest[pos + needle.len()..].trim_start();
        if let Some(val) = after.strip_prefix(':') {
            return Some(val.trim_start());
        }
        rest = &rest[pos + needle.len()..];
    }
    None
}

fn json_str(obj: &str, key: &str) -> Option<String> {
    let val = find_json_value(obj, key)?;
    let bytes = val.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = val[1..].chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16)
                        && let Some(decoded) = char::from_u32(code)
                    {
                        out.push(decoded);
                    }
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

fn json_f64(obj: &str, key: &str) -> Option<f64> {
    let val = find_json_value(obj, key)?;
    let end = val
        .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+' | 'e' | 'E'))
        .unwrap_or(val.len());
    if end == 0 {
        return None;
    }
    val[..end].parse::<f64>().ok()
}

/// Renders the floating AI Limits popover anchored near the top-right, right under the top bar.
pub fn render_popover(
    grid_cols: u16,
    grid_rows: u16,
    theme: Theme,
    snap: &Snapshot,
    active_agent: Option<Agent>,
    selected_row: Option<usize>,
    refreshing: bool,
    anchor_right_col: Option<u16>,
) -> Option<Popover> {
    if grid_cols < 26 || grid_rows < 5 {
        return None;
    }

    let bg = theme.bg.pack();
    let fg = theme.fg.pack();
    let muted = theme.tab_fg.pack();
    let accent = theme.accent.pack();
    let health = snap.health();

    let horizontal = grid_cols >= 52 && grid_rows >= 6;
    if horizontal {
        render_horizontal(
            grid_cols,
            grid_rows,
            bg,
            fg,
            muted,
            accent,
            snap,
            active_agent,
            selected_row,
            refreshing,
            anchor_right_col,
            health,
        )
    } else {
        render_compact(
            grid_cols,
            grid_rows,
            bg,
            fg,
            muted,
            accent,
            snap,
            active_agent,
            selected_row,
            refreshing,
            anchor_right_col,
            health,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn render_horizontal(
    grid_cols: u16,
    grid_rows: u16,
    bg: u32,
    fg: u32,
    muted: u32,
    accent: u32,
    snap: &Snapshot,
    active_agent: Option<Agent>,
    selected_row: Option<usize>,
    refreshing: bool,
    anchor_right_col: Option<u16>,
    health: Health,
) -> Option<Popover> {
    let entries_shown = snap.entries.len().clamp(1, MAX_VISIBLE_ENTRIES).min(usize::from(grid_rows - 2));
    let body_rows = entries_shown.max(4) as u16;
    let rows = (body_rows + 2).min(grid_rows);
    let max_cols = grid_cols.saturating_sub(2).min(58);
    let cols = max_cols.max(50).min(grid_cols);

    let right_edge = anchor_right_col
        .map(|col| col.min(grid_cols.saturating_sub(1)))
        .unwrap_or_else(|| grid_cols.saturating_sub(1));
    let origin_col = right_edge.saturating_sub(cols).min(grid_cols.saturating_sub(cols));
    let origin_row = 0;

    let mut cells = vec![Cell::blank(fg, bg); usize::from(cols) * usize::from(rows)];
    let width = usize::from(cols);

    // Left hero block: 3-layer icon + Title + Subtitle + subtle refresh trigger.
    let mid_row = rows / 2;
    let title_row = mid_row.saturating_sub(1);
    let sub_row = title_row + 1;
    let hero_row = title_row.saturating_sub(1);
    let hero_icon = (2u16, hero_row, 4u16, 3u16.min(rows.saturating_sub(hero_row)));

    let text_col = 7u16;
    let divider_col = 22u16;
    let title_room = usize::from(divider_col.saturating_sub(text_col + 1));
    paint(&mut cells, width, title_row, text_col, &fit(&snap.title, title_room), fg, bg, BOLD);

    let sub_text = selected_row
        .and_then(|idx| snap.entries.get(idx))
        .and_then(|entry| {
            if let Some(sub) = &entry.sub {
                Some(format!("7d {}% · {} {}%", entry.percent, sub.label, sub.percent))
            } else {
                entry.hint.as_deref().map(|h| format!("{} · {h}", entry.label))
            }
        })
        .unwrap_or_else(|| snap.subtitle.clone());
    paint(&mut cells, width, sub_row, text_col, &fit(&sub_text, title_room), muted, bg, 0);

    // Subtle refresh action at bottom-left of hero block.
    let refresh_row = rows.saturating_sub(1);
    let refresh_mark = if refreshing { "…" } else { "↻" };
    paint(&mut cells, width, refresh_row, text_col, refresh_mark, if refreshing { accent } else { muted }, bg, DIM);
    let refresh_zone = Some((refresh_row, text_col.saturating_sub(1), text_col + 3));

    // Right column: dot + label + GPU gradient bar + percentage.
    let right_start = divider_col + 2;
    let pct_width = 4u16; // "100%"
    let pct_col = cols.saturating_sub(pct_width + 2);
    let dot_col = right_start;
    let label_col = dot_col + 2;
    let label_width = 9u16;
    let bar_col = label_col + label_width + 1;
    let bar_cols = pct_col.saturating_sub(bar_col + 1).max(4);

    let mut meters = Vec::with_capacity(entries_shown);
    let mut row_zones = Vec::with_capacity(entries_shown);

    if snap.entries.is_empty() {
        let r = title_row;
        paint(&mut cells, width, r, right_start, "No quota data yet", fg, bg, BOLD);
        paint(&mut cells, width, r + 1, right_start, "Write ~/.config/rustty/limits.json", muted, bg, DIM);
    } else {
        let start_row = (rows.saturating_sub(entries_shown as u16)) / 2;
        for (idx, entry) in snap.entries.iter().take(entries_shown).enumerate() {
            let row = start_row + idx as u16;
            let active = active_agent.is_some() && entry.agent == active_agent;
            let selected = selected_row == Some(idx);
            let label_flags = if active || selected { BOLD } else { 0 };
            let label_fg = if selected { accent } else { fg };
            paint(
                &mut cells,
                width,
                row,
                label_col,
                &fit(&entry.label, usize::from(label_width)),
                label_fg,
                bg,
                label_flags,
            );

            let right_label = if selected {
                if let Some(sub) = &entry.sub {
                    format!("{:>3}%", sub.percent)
                } else if let Some(hint) = &entry.hint {
                    format!("{:>4}", fit(hint, 4))
                } else {
                    format!("{:>3}%", entry.percent)
                }
            } else {
                format!("{:>3}%", entry.percent)
            };
            let eff = entry.effective_percent();
            let pct_fg = if eff < 20 {
                CRIT_COLOR.pack()
            } else if eff < 45 {
                WARN_COLOR.pack()
            } else {
                fg
            };
            paint(&mut cells, width, row, pct_col, &right_label, pct_fg, bg, BOLD);

            let bar_color = meter_color(entry.percent, entry.color);
            let sub = entry.sub.as_ref().map(|s| {
                (f32::from(s.percent) / 100.0, meter_color(s.percent, s.color))
            });
            meters.push(MeterLayout {
                row,
                dot_col,
                bar_col,
                bar_cols,
                fill: f32::from(entry.percent) / 100.0,
                color: bar_color,
                sub,
            });
            row_zones.push((row, right_start, cols.saturating_sub(1), idx));
        }
    }

    Some(Popover {
        origin_col,
        origin_row,
        cols,
        rows,
        cells,
        hero_icon,
        divider_v: Some((divider_col, 1, rows.saturating_sub(1))),
        divider_h: None,
        meters,
        health,
        refresh_zone,
        row_zones,
    })
}

#[allow(clippy::too_many_arguments)]
fn render_compact(
    grid_cols: u16,
    grid_rows: u16,
    bg: u32,
    fg: u32,
    muted: u32,
    accent: u32,
    snap: &Snapshot,
    active_agent: Option<Agent>,
    selected_row: Option<usize>,
    refreshing: bool,
    anchor_right_col: Option<u16>,
    health: Health,
) -> Option<Popover> {
    let cols = grid_cols.saturating_sub(2).min(38).max(24).min(grid_cols);
    let max_entries = usize::from(grid_rows.saturating_sub(4)).min(MAX_VISIBLE_ENTRIES);
    let entries_shown = snap.entries.len().min(max_entries);
    let rows = (3 + entries_shown.max(1) as u16 + 1).min(grid_rows);

    let right_edge = anchor_right_col
        .map(|col| col.min(grid_cols.saturating_sub(1)))
        .unwrap_or_else(|| grid_cols.saturating_sub(1));
    let origin_col = right_edge.saturating_sub(cols).min(grid_cols.saturating_sub(cols));
    let origin_row = 0;

    let mut cells = vec![Cell::blank(fg, bg); usize::from(cols) * usize::from(rows)];
    let width = usize::from(cols);

    let hero_icon = (1u16, 1u16, 3u16, 1u16);
    paint(&mut cells, width, 1, 5, &fit(&snap.title, usize::from(cols.saturating_sub(9))), fg, bg, BOLD);
    let refresh_col = cols.saturating_sub(3);
    let refresh_mark = if refreshing { "…" } else { "↻" };
    paint(&mut cells, width, 1, refresh_col, refresh_mark, if refreshing { accent } else { muted }, bg, 0);
    let refresh_zone = Some((1, refresh_col.saturating_sub(1), cols));

    let dot_col = 2u16;
    let label_col = 4u16;
    let label_width = 8u16;
    let pct_col = cols.saturating_sub(5);
    let bar_col = label_col + label_width + 1;
    let bar_cols = pct_col.saturating_sub(bar_col + 1).max(3);

    let mut meters = Vec::with_capacity(entries_shown);
    let mut row_zones = Vec::with_capacity(entries_shown);

    if snap.entries.is_empty() {
        paint(&mut cells, width, 3, 2, &fit("No quota data yet", usize::from(cols - 4)), muted, bg, 0);
    } else {
        for (idx, entry) in snap.entries.iter().take(entries_shown).enumerate() {
            let row = 3 + idx as u16;
            let active = active_agent.is_some() && entry.agent == active_agent;
            let selected = selected_row == Some(idx);
            paint(
                &mut cells,
                width,
                row,
                label_col,
                &fit(&entry.label, usize::from(label_width)),
                if selected { accent } else { fg },
                bg,
                if active || selected { BOLD } else { 0 },
            );
            let right_label = if selected && let Some(sub) = &entry.sub {
                format!("{:>3}%", sub.percent)
            } else {
                format!("{:>3}%", entry.percent)
            };
            paint(&mut cells, width, row, pct_col, &right_label, fg, bg, BOLD);
            let sub = entry.sub.as_ref().map(|s| {
                (f32::from(s.percent) / 100.0, meter_color(s.percent, s.color))
            });
            meters.push(MeterLayout {
                row,
                dot_col,
                bar_col,
                bar_cols,
                fill: f32::from(entry.percent) / 100.0,
                color: meter_color(entry.percent, entry.color),
                sub,
            });
            row_zones.push((row, 1, cols.saturating_sub(1), idx));
        }
    }

    Some(Popover {
        origin_col,
        origin_row,
        cols,
        rows,
        cells,
        hero_icon,
        divider_v: None,
        divider_h: Some((2, 2, cols.saturating_sub(2))),
        meters,
        health,
        refresh_zone,
        row_zones,
    })
}

fn meter_color(percent: u8, base: Rgb) -> Rgb {
    if percent < 15 {
        CRIT_COLOR
    } else {
        base
    }
}

fn fit(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return text.chars().take(max_chars).collect();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

fn paint(cells: &mut [Cell], width: usize, row: u16, col: u16, text: &str, fg: u32, bg: u32, flags: u32) {
    let mut cursor = usize::from(col);
    for ch in text.chars() {
        if cursor >= width {
            break;
        }
        let idx = usize::from(row) * width + cursor;
        if let Some(cell) = cells.get_mut(idx) {
            *cell = Cell { cp: ch as u32, fg, bg, flags };
        }
        cursor += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_primary_and_secondary_rate_limits() {
        let sample = r#"{"type":"event_msg","payload":{"rate_limits":{"limit_id":"codex","primary":{"used_percent":18.0,"window_minutes":300,"resets_at":2000007200},"secondary":{"used_percent":63.0,"window_minutes":10080,"resets_at":2000172800}}}}"#;
        let entries = parse_codex_rate_limits(sample, 2_000_000_000);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Codex");
        assert_eq!(entries[0].percent, 37);
        assert_eq!(entries[0].hint.as_deref(), Some("2d"));
        let sub = entries[0].sub.as_ref().expect("5h sub limit");
        assert_eq!(sub.label, "5h");
        assert_eq!(sub.percent, 82);
        assert_eq!(sub.hint.as_deref(), Some("2h"));

        // Past reset timestamp resets remaining quota to 100%.
        let reset = parse_codex_rate_limits(sample, 2_100_000_000);
        assert_eq!(reset[0].percent, 100);
        assert_eq!(reset[0].sub.as_ref().expect("sub").percent, 100);
    }

    #[test]
    fn parses_custom_json_object_and_array() {
        let mut snap = Snapshot::default();
        let mut seen = HashSet::new();
        let payload = r##"{
            "title": "AI Limits",
            "subtitle": "Quota · Models",
            "items": [
                {"id": "g4n", "label": "G4N", "percent": 100, "color": "#18D2FF"},
                {"id": "g4c", "label": "G4C", "percent": 82, "color": "#7C6CFF"},
                {"id": "argon", "label": "Argon", "percent": 64, "color": "#1EE6A8"},
                {"id": "claude", "label": "Claude", "used_percent": 63, "color": "#FB8C45", "reset": "3h10m", "sub": {"label": "5h", "used_percent": 15, "reset": "1h20m"}}
            ]
        }"##;
        apply_json(payload, 2_000_000_000, &mut snap, &mut seen);
        assert_eq!(snap.entries.len(), 4);
        assert_eq!(snap.entries[0].label, "G4N");
        assert_eq!(snap.entries[0].percent, 100);
        assert_eq!(snap.entries[3].label, "Claude");
        assert_eq!(snap.entries[3].percent, 37);
        assert_eq!(snap.entries[3].hint.as_deref(), Some("3h10m"));
        let sub = snap.entries[3].sub.as_ref().expect("claude 5h sub");
        assert_eq!(sub.percent, 85);
        assert_eq!(sub.hint.as_deref(), Some("1h20m"));
        assert_eq!(snap.health(), Health::Warn);
    }

    #[test]
    fn parses_claude_and_antigravity_context_lines() {
        let agy = r#"{"model":"gemini-4-neon-high","input_tokens":180000,"cache_read_tokens":20000,"output_tokens":500}"#;
        let entry = parse_agy_transcript(agy).expect("agy");
        assert_eq!(entry.label, "Gemini");
        assert_eq!(entry.percent, 80);

        let claude = r#"{"message":{"model":"claude-sonnet-5-5","usage":{"input_tokens":1000,"cache_creation_input_tokens":9000,"cache_read_input_tokens":240000}}}"#;
        let entry = parse_claude_session(claude).expect("claude");
        assert_eq!(entry.label, "Claude");
        assert_eq!(entry.percent, 75);

        let cursor = cursor_entry_from_bytes(160_000);
        assert_eq!(cursor.label, "Cursor");
        assert_eq!(cursor.percent, 80);
        assert_eq!(cursor.agent, Some(Agent::Cursor));
    }

    #[test]
    fn horizontal_and_compact_popovers_fit_and_hit_test() {
        let theme = crate::color::theme(0);
        let snap = Snapshot {
            title: "AI Limits".into(),
            subtitle: "Quota · Models".into(),
            entries: vec![
                Entry { id: "g4n".into(), label: "G4N".into(), percent: 100, color: PALETTE[0], hint: None, sub: None, agent: Some(Agent::Antigravity) },
                Entry { id: "claude".into(), label: "Claude".into(), percent: 18, color: PALETTE[3], hint: Some("2h".into()), sub: None, agent: Some(Agent::Claude) },
            ],
            updated_at: None,
        };
        let wide = render_popover(90, 24, theme, &snap, Some(Agent::Claude), None, false, Some(88)).expect("wide");
        assert!(wide.divider_v.is_some());
        assert_eq!(wide.meters.len(), 2);
        assert_eq!(wide.health, Health::Crit);
        assert!(wide.contains(wide.origin_col, wide.origin_row));
        let row0 = wide.origin_row + wide.meters[0].row;
        let col0 = wide.origin_col + wide.meters[0].bar_col;
        assert_eq!(wide.hit(col0, row0), Some(PopoverHit::Row(0)));

        let narrow = render_popover(36, 16, theme, &snap, None, None, false, Some(35)).expect("narrow");
        assert!(narrow.divider_h.is_some());
        assert_eq!(narrow.meters.len(), 2);
    }
}
