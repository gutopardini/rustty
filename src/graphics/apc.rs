use memchr::{memchr3, memmem};

use crate::term::Term;

// Cada APC carrega no máximo 4096 bytes de imagem. Reserve espaço para os parâmetros.
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
    // vte descarta o conteúdo dos APCs. Intercepte seu payload mantendo o estado VT ao redor.
    pub(crate) fn advance(&mut self, term: &mut Term, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            match self.state {
                State::Text => {
                    let Some(start) = memmem::find(bytes, b"\x1b_") else {
                        self.vte.advance(term, bytes);
                        if bytes.last() == Some(&ESC) {
                            self.state = State::Escape;
                        }
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
                            term.graphics_command(command);
                        }
                    }
                    self.clear_apc();
                }
            }
        }
    }

    fn clear_apc(&mut self) {
        self.apc.clear();
        self.overflow = false;
    }
}
