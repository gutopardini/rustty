//! Rustty é um terminal Linux escrito do zero.
//!
//! O desenho segue metas de terminais acelerados por GPU: cache de glifos em
//! VRAM, parser fora do caminho de apresentação e upload só das linhas que
//! mudaram.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("Rustty só compila em Linux");

pub mod agent;
pub mod app;
pub mod cell;
pub mod color;
pub mod confirm;
pub mod font;
mod ft;
pub mod gpu;
mod graphics;
pub mod input;
pub mod npu;
pub mod pty;
mod scope;
pub mod settings;
pub mod simd;
pub mod tabs;
pub mod term;
pub mod titlebar;

pub use cell::{Cell, BOLD, DIM, ITALIC, REVERSE, SPACER, UNDERLINE, WIDE};
pub use term::Term;
