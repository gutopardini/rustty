//! Rustty é um terminal Linux escrito do zero.
//!
//! O desenho segue metas públicas de terminais acelerados por GPU: cache de
//! glifos em VRAM, parser fora do caminho de apresentação e upload só das
//! linhas que mudaram. Não é um porte do código do Kitty.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("Rustty só compila em Linux");

pub mod app;
pub mod cell;
pub mod color;
pub mod font;
pub mod gpu;
pub mod input;
pub mod npu;
pub mod pty;
pub mod simd;
pub mod term;

pub use cell::{Cell, BOLD, DIM, ITALIC, REVERSE, SPACER, UNDERLINE, WIDE};
pub use term::Term;
