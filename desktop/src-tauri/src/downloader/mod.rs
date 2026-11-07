//! The downloader, being ported from the Python engine (core/downloader.py and friends) piece by
//! piece. Each piece is checked against the Python version's answers (tests/fixtures/) before
//! the Rust downloader starts using it.

#![allow(dead_code)] // used once the Rust downloader takes over from the Python one

pub mod extract;
pub mod filters;
pub mod links;
pub mod naming;
pub mod order;
pub mod plan;
pub mod sites;
mod py;

#[cfg(test)]
mod golden;
