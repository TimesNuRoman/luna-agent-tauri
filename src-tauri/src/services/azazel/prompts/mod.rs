//! Azazel prompt assets.
//!
//! The actual prompt text lives in sibling `.txt` files and is pulled
//! into the binary at compile time via `include_str!` from
//! `super::supervisor`. This `mod.rs` exists only so the parent
//! `services::azazel::mod.rs` can declare `pub mod prompts;` without
//! tripping Rust's missing-module check.
