//! Stress the path a live tab actually runs: parse a burst, publish the grid,
//! hash the lines that left the screen, then resolve glyphs for the GPU.
//!
//! `rustty-bench` times one `advance` of a flat buffer. This binary also
//! times the reader loop (64 KiB bursts, snapshot, scrollback index) and the
//! atlas lookup that runs on every dirty cell before upload.

use std::hint::black_box;
use std::time::{Duration, Instant};

use rustty::font::{Atlas, Face};
use rustty::Term;

const COLS: u16 = 200;
const ROWS: u16 = 50;
const BULK: usize = 4 * 1024 * 1024;
const BURST: usize = 64 * 1024;

struct Sample {
    name: &'static str,
    bytes: usize,
    parse: Duration,
    publish: Duration,
    index: Duration,
    cells_copied: u64,
    full_frames: u64,
    partial_frames: u64,
    lines_indexed: u64,
}

fn main() {
    let cells = usize::from(COLS) * usize::from(ROWS);
    println!("grid {COLS}x{ROWS}  ({cells} cells, {} KiB per full frame)", cells * 16 / 1024);
    println!();

    println!("parser ceiling — one advance, one snapshot");
    println!("{:<18} {:>8} {:>9} {:>9} {:>9} {:>8} {:>10}", "workload", "MiB", "parse", "publish", "MiB/s", "full", "copied");
    for (name, bytes) in ceilings() {
        let sample = run(name, &bytes, bytes.len());
        print_sample(&sample);
    }
    println!();

    println!("reader loop — 64 KiB bursts, snapshot and index after each burst");
    println!(
        "{:<18} {:>8} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8} {:>10}",
        "workload", "MiB", "parse", "publish", "index", "MiB/s", "full", "partial", "indexed", "copied"
    );
    for (name, bytes) in bursts() {
        let sample = run(name, &bytes, BURST);
        print_reader(&sample);
    }
    println!();

    frame_budget();
    atlas_budget();
}

fn ceilings() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("ascii", ascii(BULK)),
        ("unicode", unicode(BULK)),
        ("sgr", sgr(BULK)),
        ("cjk", cjk(BULK)),
        ("cursor", cursor_storm(80_000)),
    ]
}

fn bursts() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("ascii", ascii(BULK)),
        ("ascii-128B", ascii(BULK)),
        ("unicode", unicode(BULK)),
        ("sgr", sgr(BULK)),
        ("cjk", cjk(BULK)),
        ("region", region_log(BULK)),
        ("alt-repaint", alt_frames(40)),
        ("status-line", status_frames(400)),
    ]
}

fn run(name: &'static str, bytes: &[u8], chunk: usize) -> Sample {
    let chunk = if name == "ascii-128B" { 128 } else { chunk.max(1) };
    let mut term = Term::new(COLS, ROWS);
    let mut parse = Duration::ZERO;
    let mut publish = Duration::ZERO;
    let mut index = Duration::ZERO;
    let mut cells_copied = 0u64;
    let mut full_frames = 0u64;
    let mut partial_frames = 0u64;
    let mut lines_indexed = 0u64;
    for piece in bytes.chunks(chunk) {
        let start = Instant::now();
        term.advance(piece);
        parse += start.elapsed();

        let start = Instant::now();
        let snap = term.snapshot();
        publish += start.elapsed();
        if snap.full {
            full_frames += 1;
            cells_copied += snap.cells.len() as u64;
        } else if !snap.rows_dirty.is_empty() {
            partial_frames += 1;
            cells_copied += snap.rows_dirty.iter().map(|(_, row)| row.len() as u64).sum::<u64>();
        }
        black_box(snap.cols);

        let start = Instant::now();
        let records = term.take_index_records();
        lines_indexed += records.len() as u64;
        black_box(records.len());
        index += start.elapsed();
    }
    Sample {
        name,
        bytes: bytes.len(),
        parse,
        publish,
        index,
        cells_copied,
        full_frames,
        partial_frames,
        lines_indexed,
    }
}

fn print_sample(sample: &Sample) {
    let mb = mib(sample.bytes);
    let total = secs(sample.parse + sample.publish);
    println!(
        "{:<18} {mb:8.2} {:>9} {:>9} {:>9.1} {:>8} {:>10}",
        sample.name,
        ms(sample.parse),
        ms(sample.publish),
        mb / total,
        sample.full_frames,
        sample.cells_copied,
    );
}

fn print_reader(sample: &Sample) {
    let mb = mib(sample.bytes);
    let total = secs(sample.parse + sample.publish + sample.index);
    println!(
        "{:<18} {mb:8.2} {:>9} {:>9} {:>9} {:>8.1} {:>8} {:>8} {:>8} {:>10}",
        sample.name,
        ms(sample.parse),
        ms(sample.publish),
        ms(sample.index),
        mb / total,
        sample.full_frames,
        sample.partial_frames,
        sample.lines_indexed,
        sample.cells_copied,
    );
}

/// One alt-screen paint and one status-line paint, then the glyph resolve that
/// `upload` does for every dirty cell. Times are per frame, against 16.6 ms
/// (60 Hz) and 6.9 ms (144 Hz).
fn frame_budget() {
    println!("frame budget — parse + publish + glyph resolve, one frame at a time");
    let face = match Face::load(15.0) {
        Ok(face) => face,
        Err(err) => {
            println!("font unavailable: {err}");
            return;
        }
    };
    let mut atlas = Atlas::new(face.cell_w(), face.cell_h());
    atlas.warm(&face);

    let paint = one_alt_paint();
    let status = one_status_paint();
    report_frame("alt full paint", &paint, &face, &mut atlas);
    report_frame("status line", &status, &face, &mut atlas);
    println!("  60 Hz budget 16.6 ms    144 Hz budget 6.9 ms");
    println!();
}

fn report_frame(name: &str, bytes: &[u8], face: &Face, atlas: &mut Atlas) {
    let mut term = Term::new(COLS, ROWS);
    // Fill once so the measured frame is a repaint, not the first blank screen.
    term.advance(&one_alt_paint());
    let _ = term.snapshot();
    let _ = term.take_index_records();

    let start = Instant::now();
    term.advance(bytes);
    let parse = start.elapsed();
    let start = Instant::now();
    let snap = term.snapshot();
    let publish = start.elapsed();
    let cells: Vec<_> = if snap.full {
        snap.cells.clone()
    } else {
        snap.rows_dirty.iter().flat_map(|(_, row)| row.iter().copied()).collect()
    };
    let dirty = cells.len();
    let start = Instant::now();
    for cell in &cells {
        black_box(atlas.prepare(face, cell.cp, cell.flags));
    }
    let resolve = start.elapsed();
    let total = parse + publish + resolve;
    println!(
        "  {name:<16} parse {}  publish {}  resolve {}  total {}  dirty {dirty}",
        ms(parse),
        ms(publish),
        ms(resolve),
        ms(total),
    );
}

fn atlas_budget() {
    println!("glyph atlas");
    let face = match Face::load(15.0) {
        Ok(face) => face,
        Err(err) => {
            println!("font unavailable: {err}");
            return;
        }
    };
    let samples = [
        ('A', false, "ascii"),
        ('A', true, "ascii bold"),
        ('你', false, "cjk"),
        ('─', false, "box"),
        ('\u{e0b0}', false, "powerline"),
        ('\u{f0e7e}', false, "nerd icon"),
    ];
    println!("  {:>12}  {:>10}  {:>10}", "glyph", "cold", "hot x1000");
    for (ch, bold, label) in samples {
        let start = Instant::now();
        black_box(face.raster_cell(ch, bold, false));
        let cold = start.elapsed();
        let start = Instant::now();
        for _ in 0..1000 {
            black_box(face.raster_cell(ch, bold, false));
        }
        let hot = start.elapsed();
        println!("  {label:>12}  {:>10}  {:>10}", ms(cold), ms(hot));
    }

    let mut atlas = Atlas::new(face.cell_w(), face.cell_h());
    let start = Instant::now();
    atlas.warm(&face);
    let warm = start.elapsed();
    let grid = usize::from(COLS) * usize::from(ROWS);
    let start = Instant::now();
    for i in 0..grid {
        let cp = u32::from(b'A') + (i % 26) as u32;
        black_box(atlas.prepare(&face, cp, 0));
    }
    let hot = start.elapsed();
    println!(
        "  warm ASCII+bold+powerline {}    hot prepare of {grid} cells {}",
        ms(warm),
        ms(hot),
    );
}

fn ascii(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let line = b"abcdefghijklmnopqrstuvwxyz0123456789 rustty stress line\r\n";
    while out.len() < n {
        out.extend_from_slice(line);
    }
    out.truncate(n);
    out
}

fn unicode(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let line = "café — não é ASCII, linha de log\r\n".as_bytes();
    while out.len() < n {
        out.extend_from_slice(line);
    }
    out.truncate(n);
    out
}

fn sgr(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let line = b"\x1b[38;2;120;180;90m\x1b[1mrustty\x1b[0m \x1b[48;5;17mcell\x1b[0m streaming line\r\n";
    while out.len() < n {
        out.extend_from_slice(line);
    }
    out.truncate(n);
    out
}

fn cjk(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let line = "终端压力 テスト 가나다 一行\r\n".as_bytes();
    while out.len() < n {
        out.extend_from_slice(line);
    }
    out.truncate(n);
    out
}

fn region_log(n: usize) -> Vec<u8> {
    let mut out = format!("\x1b[1;{}r", ROWS - 1).into_bytes();
    let line = b"region scroll keeps a status row pinned\r\n";
    while out.len() < n {
        out.extend_from_slice(line);
    }
    out.truncate(n);
    out
}

fn cursor_storm(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 16);
    for i in 0..n {
        let row = (i % usize::from(ROWS)) + 1;
        let col = (i % usize::from(COLS)) + 1;
        let ch = b'A' + (i % 26) as u8;
        out.extend(format!("\x1b[{row};{col}H{}", ch as char).into_bytes());
    }
    out
}

fn alt_frames(frames: usize) -> Vec<u8> {
    let mut out = b"\x1b[?1049h".to_vec();
    for frame in 0..frames {
        out.extend(one_frame(frame).into_bytes());
    }
    out
}

fn one_alt_paint() -> Vec<u8> {
    let mut out = b"\x1b[?1049h".to_vec();
    out.extend(one_frame(1).into_bytes());
    out
}

fn one_frame(frame: usize) -> String {
    let mut out = String::from("\x1b[H");
    for row in 0..usize::from(ROWS) {
        let tone = ((frame + row) % 200) as u8;
        out.push_str(&format!("\x1b[{};1H\x1b[38;2;{tone};140;90m", row + 1));
        for col in 0..usize::from(COLS) {
            out.push((b'A' + ((col + row + frame) % 26) as u8) as char);
        }
    }
    out
}

fn status_frames(frames: usize) -> Vec<u8> {
    let mut out = one_alt_paint();
    for frame in 0..frames {
        out.extend(status_line(frame).into_bytes());
    }
    out
}

fn one_status_paint() -> Vec<u8> {
    status_line(7).into_bytes()
}

fn status_line(frame: usize) -> String {
    let row = usize::from(ROWS);
    let mut body = format!("status {frame:04} ");
    while body.len() < usize::from(COLS) {
        body.push('#');
    }
    body.truncate(usize::from(COLS));
    format!("\x1b[{row};1H\x1b[38;2;220;180;40m{body}")
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn secs(duration: Duration) -> f64 {
    duration.as_secs_f64().max(1e-9)
}

fn ms(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
}
