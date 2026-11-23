//! WASM entry point for Zellij.

#![cfg_attr(
    target_arch = "wasm32",
    expect(missing_docs, reason = "Zellij generates the ABI exports")
)]

#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::*;

#[cfg(target_arch = "wasm32")]
register_plugin!(zj_status_toolbar::Toolbar);

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("Build for wasm32-wasip1 and load the resulting plugin in Zellij.");
}
