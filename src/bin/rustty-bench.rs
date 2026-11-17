use std::time::Instant;

use rustty::Term;

fn main() {
    let megabytes = 8usize;
    report("ascii", &ascii(megabytes));
    report("unicode", &unicode(megabytes));
    report("sgr", &sgr(megabytes));
}

fn report(name: &str, bytes: &[u8]) {
    let mut term = Term::new(120, 40);
    let start = Instant::now();
    term.advance(bytes);
    let elapsed = start.elapsed().as_secs_f64().max(1e-9);
    let mb = bytes.len() as f64 / (1024.0 * 1024.0);
    println!("{name:<8} {mb:6.2} MiB em {elapsed:7.3} s   {:>7.1} MiB/s", mb / elapsed);
}

fn ascii(megabytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(megabytes * 1024 * 1024);
    let line = b"abcdefghijklmnopqrstuvwxyz0123456789\r\n";
    while out.len() < megabytes * 1024 * 1024 {
        out.extend_from_slice(line);
    }
    out
}

fn unicode(megabytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(megabytes * 1024 * 1024);
    let line = "ação não é ASCII — 你\r\n".as_bytes();
    while out.len() < megabytes * 1024 * 1024 {
        out.extend_from_slice(line);
    }
    out
}

fn sgr(megabytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(megabytes * 1024 * 1024);
    let line = b"\x1b[38;2;120;180;90m\x1b[1mrustty\x1b[0m \x1b[48;5;17mcell\x1b[0m\r\n";
    while out.len() < megabytes * 1024 * 1024 {
        out.extend_from_slice(line);
    }
    out
}
