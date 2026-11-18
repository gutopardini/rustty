//! Rustty is a performance-focused CLI for LLMs.
//!
//! The terminal is written in Rust. The cell grid is drawn on the GPU: glyphs
//! stay in VRAM, the parser stays off the present path, and only dirty rows
//! are uploaded. Scrollback is staged for the Intel NPU.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("Rustty builds on Linux only");

pub mod agent;
pub mod app;
pub mod cell;
mod clipboard;
pub mod color;
pub mod confirm;
pub mod desktop;
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
