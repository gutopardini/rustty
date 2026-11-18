use std::collections::VecDeque;
use std::io::Cursor;
use std::ops::Range;
use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};

mod apc;

pub(crate) use apc::Parser;

pub(crate) const MAX_PLACEMENTS: usize = 32;
const MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 2048;
const MAX_SCREEN_BYTES: usize = 32 * 1024 * 1024;

pub(crate) struct Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct Placement {
    id: u32,
    pub(crate) col: u16,
    pub(crate) row: i32,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) visible_rows: Range<i32>,
    pub(crate) image: Arc<Image>,
}

#[derive(Default)]
pub(crate) struct Graphics {
    pub(crate) placements: VecDeque<Placement>,
    upload: Option<Upload>,
}

struct Upload {
    command: Command,
    encoded: Vec<u8>,
}

pub(crate) enum Update {
    Unchanged,
    Changed,
    MoveCursor { cols: u16, rows: u16 },
}

pub(crate) enum Prepared {
    Update(Update),
    Decode(DecodeJob),
}

pub(crate) struct DecodeJob {
    command: Command,
    encoded: Vec<u8>,
    row: u16,
    col: u16,
}

impl Graphics {
    pub(crate) fn command(
        &mut self,
        data: &[u8],
        row: u16,
        col: u16,
        replies: &mut Vec<u8>,
    ) -> Prepared {
        let pending = self.upload.take();
        let mut parts = data.splitn(2, |byte| *byte == b';');
        let Some(command) = Command::parse(parts.next().unwrap_or_default()) else {
            return Prepared::Update(Update::Unchanged);
        };
        let payload = parts.next().unwrap_or_default();

        if command.action == b'd' {
            let old_len = self.placements.len();
            match command.delete {
                b'a' | b'A' => self.placements.clear(),
                b'i' | b'I' if command.id != 0 => {
                    self.placements
                        .retain(|placement| placement.id != command.id);
                }
                _ => {}
            }
            return Prepared::Update(if self.placements.len() == old_len {
                Update::Unchanged
            } else {
                Update::Changed
            });
        }

        let mut upload = if command.continuation {
            let Some(mut pending) = pending else {
                return Prepared::Update(Update::Unchanged);
            };
            if command.quiet.is_some() {
                pending.command.quiet = command.quiet;
            }
            pending
        } else {
            if !matches!(command.action, b'T' | b'q') {
                return Prepared::Update(Update::Unchanged);
            }
            if command.medium != b'd' {
                command.reply(replies, "EINVAL:unsupported transmission medium");
                return Prepared::Update(Update::Unchanged);
            }
            Upload {
                command,
                encoded: Vec::new(),
            }
        };

        if upload.encoded.len() + payload.len() > MAX_ENCODED_BYTES {
            upload
                .command
                .reply(replies, "EFBIG:image payload exceeds limit");
            return Prepared::Update(Update::Unchanged);
        }
        upload.encoded.extend_from_slice(payload);
        if command.more {
            self.upload = Some(upload);
            return Prepared::Update(Update::Unchanged);
        }

        Prepared::Decode(DecodeJob {
            command: upload.command,
            encoded: upload.encoded,
            row,
            col,
        })
    }

    pub(crate) fn finish(&mut self, job: DecodeJob, image: Option<Image>, replies: &mut Vec<u8>) -> Update {
        let DecodeJob { command, row, col, .. } = job;
        let Some(image) = image else {
            command.reply(replies, "EINVAL:invalid or unsupported image");
            return Update::Unchanged;
        };
        if command.action == b'q' {
            command.reply(replies, "OK");
            return Update::Unchanged;
        }
        if command.cols == 0 || command.rows == 0 {
            command.reply(replies, "EINVAL:placement requires columns and rows");
            return Update::Unchanged;
        }
        command.reply(replies, "OK");

        if command.id != 0 {
            self.placements
                .retain(|placement| placement.id != command.id);
        }
        let mut bytes: usize = self.placements.iter().map(|p| p.image.rgba.len()).sum();
        while self.placements.len() >= MAX_PLACEMENTS || bytes + image.rgba.len() > MAX_SCREEN_BYTES
        {
            bytes -= self.placements.pop_front().unwrap().image.rgba.len();
        }

        let row = i32::from(row);
        self.placements.push_back(Placement {
            id: command.id,
            col,
            row,
            cols: command.cols,
            rows: command.rows,
            visible_rows: row..row + i32::from(command.rows),
            image: Arc::new(image),
        });
        if command.keep_cursor {
            Update::Changed
        } else {
            Update::MoveCursor {
                cols: command.cols,
                rows: command.rows,
            }
        }
    }

    pub(crate) fn scroll(&mut self, top: u16, bottom: u16, lines: i16, screen_rows: u16) {
        let top = i32::from(top);
        let bottom = i32::from(bottom) + 1;
        let full_screen = top == 0 && bottom == i32::from(screen_rows);
        let shift = i32::from(lines).clamp(top - bottom, bottom - top);
        self.placements.retain_mut(|placement| {
            let visible = &mut placement.visible_rows;
            if !full_screen && (visible.start < top || visible.end > bottom) {
                return true;
            }
            placement.row -= shift;
            visible.start = (visible.start - shift).max(top);
            visible.end = (visible.end - shift).min(bottom);
            visible.start < visible.end
        });
    }
}

#[derive(Clone, Copy)]
struct Command {
    action: u8,
    medium: u8,
    format: u8,
    delete: u8,
    id: u32,
    width: u32,
    height: u32,
    cols: u16,
    rows: u16,
    quiet: Option<u8>,
    more: bool,
    keep_cursor: bool,
    continuation: bool,
}

impl Command {
    fn parse(header: &[u8]) -> Option<Self> {
        let mut command = Self {
            action: b't',
            medium: b'd',
            format: 32,
            delete: b'a',
            id: 0,
            width: 0,
            height: 0,
            cols: 0,
            rows: 0,
            quiet: None,
            more: false,
            keep_cursor: false,
            continuation: true,
        };
        if header.is_empty() {
            return Some(command);
        }
        for pair in std::str::from_utf8(header).ok()?.split(',') {
            let (key, value) = pair.split_once('=')?;
            let flag = || match value.as_bytes() {
                [byte] => Some(*byte),
                _ => None,
            };
            command.continuation &= matches!(key, "m" | "q");
            match key {
                "a" => command.action = flag()?,
                "t" => command.medium = flag()?,
                "f" => command.format = value.parse().ok()?,
                "d" => command.delete = flag()?,
                "i" => command.id = value.parse().ok()?,
                "s" => command.width = value.parse().ok()?,
                "v" => command.height = value.parse().ok()?,
                "c" => command.cols = value.parse().ok()?,
                "r" => command.rows = value.parse().ok()?,
                "q" => command.quiet = Some(value.parse().ok()?),
                "m" => command.more = flag()? == b'1',
                "C" => command.keep_cursor = flag()? == b'1',
                _ => return None,
            }
        }
        Some(command)
    }

    fn reply(self, replies: &mut Vec<u8>, message: &str) {
        let quiet = self.quiet.unwrap_or_default();
        if self.id != 0 && quiet < 2 && (quiet == 0 || message != "OK") {
            replies.extend_from_slice(format!("\x1b_Gi={};{message}\x1b\\", self.id).as_bytes());
        }
    }
}

pub(crate) fn decode_job(job: &DecodeJob) -> Option<Image> {
    decode(&job.command, &job.encoded)
}

fn decode(command: &Command, encoded: &[u8]) -> Option<Image> {
    let bytes = STANDARD.decode(encoded).ok()?;
    if command.format == 100 {
        return decode_png(bytes);
    }
    let channels = match command.format {
        24 => 3,
        32 => 4,
        _ => return None,
    };
    let width = command.width;
    let height = command.height;
    if !valid_dimensions(width, height) || bytes.len() != (width * height * channels) as usize {
        return None;
    }
    let rgba = if channels == 4 {
        bytes
    } else {
        bytes
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect()
    };
    Some(Image {
        width,
        height,
        rgba,
    })
}

fn decode_png(bytes: Vec<u8>) -> Option<Image> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: MAX_SCREEN_BYTES,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_ignore_text_chunk(true);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if !valid_dimensions(info.width, info.height) || info.animation_control.is_some() {
        return None;
    }
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    reader.finish().ok()?;
    pixels.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => pixels,
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&p| [p, p, p, 255]).collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => return None,
    };
    Some(Image {
        width: frame.width,
        height: frame.height,
        rgba,
    })
}

fn valid_dimensions(width: u32, height: u32) -> bool {
    (1..=MAX_IMAGE_DIMENSION).contains(&width) && (1..=MAX_IMAGE_DIMENSION).contains(&height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Term;

    const RED: &[u8] = b"\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=2,q=2,C=1;/wAA/w==\x1b\\";
    const BLUE: &[u8] = b"\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=2,q=2,C=1;AAD//w==\x1b\\";

    #[test]
    fn chunked_png_survives_every_pty_split() {
        let rgba = [255, 96, 0, 128];
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&rgba)
            .unwrap();
        let encoded = STANDARD.encode(png);
        let (first, last) = encoded.split_at(16);
        let input = format!(
            "é\x1b[2;4H\x1b_Ga=T,t=d,f=100,i=49374,c=2,r=2,q=2,m=1,C=1;{first}\x1b\\\x1b_Gm=0;{last}\x1b\\x"
        );
        let input = input.as_bytes();

        for split in 0..=input.len() {
            let mut term = Term::new(10, 5);
            term.advance(&input[..split]);
            term.advance(&input[split..]);
            let snap = term.snapshot();
            let [image] = snap.images.as_slice() else {
                panic!("expected one image at PTY split {split}");
            };
            assert_eq!((image.col, image.row), (3, 1));
            assert_eq!(image.image.rgba, rgba);
            assert_eq!(snap.cells[0].cp, 'é' as u32);
            assert_eq!(snap.cells[13].cp, 'x' as u32);
            assert_eq!(snap.cursor, Some((4, 1)));
            assert!(term.take_replies().is_empty());
        }
    }

    #[test]
    fn codex_pet_frame_lands_on_the_saved_cursor_and_replaces_by_id() {
        let rgba = [255, 96, 0, 128];
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&rgba)
            .unwrap();
        let encoded = STANDARD.encode(png);
        let frame = format!(
            "\x1b_Ga=d,d=I,i=49374,q=2;\x1b\\\x1b7\x1b[4;3H\x1b_Ga=T,t=d,f=100,c=4,r=5,q=2,i=49374,m=0,C=1;{encoded}\x1b\\\x1b8"
        );

        let mut term = Term::new(20, 8);
        term.advance(b"\x1b[2;5H");
        term.advance(frame.as_bytes());
        let snap = term.snapshot();
        assert!(term.take_replies().is_empty());
        assert_eq!(snap.cursor, Some((4, 1)));
        let [image] = snap.images.as_slice() else {
            panic!("expected the pet sprite");
        };
        assert_eq!((image.col, image.row), (2, 3));
        assert_eq!((image.cols, image.rows), (4, 5));
        assert_eq!(image.image.rgba, rgba);

        term.advance(b"\x1b_Ga=d,d=I,i=49374,q=2;\x1b\\");
        assert!(term.snapshot().images.is_empty());
    }

    #[test]
    fn replacement_and_delete_invalidate_the_frame_without_text_damage() {
        let mut term = Term::new(8, 4);
        term.snapshot();
        term.advance(RED);
        let first = term.snapshot();
        assert!(first.changed);
        assert!(first.rows_dirty.is_empty());
        assert_eq!(first.images[0].image.rgba, [255, 0, 0, 255]);

        term.advance(BLUE);
        let replaced = term.snapshot();
        assert_eq!(replaced.images.len(), 1);
        assert_eq!(replaced.images[0].image.rgba, [0, 0, 255, 255]);

        term.advance(b"\x1b_Ga=d,d=I,i=1,q=2;\x1b\\");
        let deleted = term.snapshot();
        assert!(deleted.changed);
        assert!(deleted.images.is_empty());
        assert!(!term.snapshot().changed);
    }

    #[test]
    fn alternate_screen_and_sync_keep_images_separate() {
        let mut term = Term::new(8, 4);
        term.advance(b"\x1b[3;4H");
        term.advance(RED);
        term.advance(b"\x1b[?1049h\x1b[?2026h\x1b[2;2H\x1b7");
        term.advance(BLUE);
        assert!(term.snapshot().suspended);
        term.advance(b"\x1b8\x1b[?2026l");
        assert_eq!(term.snapshot().images[0].image.rgba, [0, 0, 255, 255]);

        term.advance(b"\x1b[?1049l");
        let primary = term.snapshot();
        assert_eq!(primary.cursor, Some((3, 2)));
        assert_eq!(primary.images[0].image.rgba, [255, 0, 0, 255]);
        term.advance(b"\x1b[2J");
        assert!(term.snapshot().images.is_empty());
        term.advance(b"\x1b[?1049h");
        assert!(term.snapshot().images.is_empty());
    }

    #[test]
    fn graphics_query_and_invalid_apc_leave_the_text_parser_in_sync() {
        let mut term = Term::new(8, 4);
        term.advance(b"\x1b_Ga=q,t=d,i=31,s=1,v=1,f=24;AAAA\x1b\\\x1b[c");
        assert_eq!(term.take_replies(), b"\x1b_Gi=31;OK\x1b\\\x1b[?6c");

        term.advance(b"\x1b_G");
        term.advance(&[b'A'; 8192]);
        term.advance(b"\x1b\\a\x1b_Ga=T,\x18b\x1b_ignored\x1b\\c");
        let snap = term.snapshot();
        assert!(snap.images.is_empty());
        assert_eq!(
            snap.cells[..3]
                .iter()
                .map(|cell| cell.cp)
                .collect::<Vec<_>>(),
            [97, 98, 99]
        );
    }

    #[test]
    fn scrolling_clips_the_image_without_restoring_erased_pixels() {
        let mut term = Term::new(8, 4);
        term.advance(RED);
        term.advance(b"\x1b[1S");
        let snap = term.snapshot();
        assert_eq!(snap.images[0].row, -1);
        assert_eq!(snap.images[0].visible_rows, 0..1);
        term.advance(b"\x1b[1T");
        let snap = term.snapshot();
        assert_eq!(snap.images[0].row, 0);
        assert_eq!(snap.images[0].visible_rows, 1..2);
    }
}
