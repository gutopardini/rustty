use memchr::{memchr3, memmem};

use crate::term::Term;

// Each APC carries at most 4096 bytes of image data. Leave room for the parameters.
const MAX_APC: usize = 4096 + 256;
const ESC: u8 = 0x1b;

#[derive(Default)]
pub(crate) struct Parser {
    vte: vte::Parser,
    state: State,
    apc: Vec<u8>,
    overflow: bool,
}

#[derive(Default)]
enum State {
    #[default]
    Text,
    Escape,
    Apc,
    ApcEscape,
}

impl Parser {
    /// Feed `bytes`. A completed Kitty graphics command is returned so the image can be
    /// decoded without holding the terminal lock across the rest of the stream.
    pub(crate) fn advance<'a>(
        &mut self,
        term: &mut Term,
        mut bytes: &'a [u8],
    ) -> (&'a [u8], Option<Vec<u8>>) {
        while !bytes.is_empty() {
            match self.state {
                State::Text => {
                    let Some(start) = memmem::find(bytes, b"\x1b_") else {
                        self.vte.advance(term, bytes);
                        if bytes.last() == Some(&ESC) {
                            self.state = State::Escape;
                        }
                        bytes = &[];
                        break;
                    };
                    self.vte.advance(term, &bytes[..start + 2]);
                    bytes = &bytes[start + 2..];
                    self.state = State::Apc;
                }
                State::Escape => {
                    self.state = State::Text;
                    if bytes[0] == b'_' {
                        self.vte.advance(term, &bytes[..1]);
                        bytes = &bytes[1..];
                        self.state = State::Apc;
                    }
                }
                State::Apc => {
                    let end = memchr3(ESC, 0x18, 0x1a, bytes).unwrap_or(bytes.len());
                    if !self.overflow {
                        self.overflow = self.apc.len() + end > MAX_APC;
                        if !self.overflow {
                            self.apc.extend_from_slice(&bytes[..end]);
                        }
                    }
                    bytes = &bytes[end..];
                    if let Some(&byte) = bytes.first() {
                        self.vte.advance(term, &bytes[..1]);
                        bytes = &bytes[1..];
                        if byte == ESC {
                            self.state = State::ApcEscape;
                        } else {
                            self.clear_apc();
                            self.state = State::Text;
                        }
                    }
                }
                State::ApcEscape => {
                    self.state = State::Escape;
                    if bytes[0] == b'\\' {
                        self.vte.advance(term, &bytes[..1]);
                        bytes = &bytes[1..];
                        self.state = State::Text;
                        if !self.overflow
                            && let Some(command) = self.apc.strip_prefix(b"G")
                        {
                            let command = command.to_vec();
                            self.clear_apc();
                            return (bytes, Some(command));
                        }
                    }
                    self.clear_apc();
                }
            }
        }
        (bytes, None)
    }

    fn clear_apc(&mut self) {
        self.apc.clear();
        self.overflow = false;
    }
}
