//! Maca: a package manager for macOS and Linux.
//!
//! Installs from five backends: Homebrew bottles and sources, the winget catalog,
//! GitHub Releases, the Aqua registry and a custom JSON registry. It calls their
//! HTTP APIs directly and never shells out to `brew`, `winget`, `gh` or `aqua`.
//! Installs download and verify artifacts themselves, and the crate also provides
//! snapshots with rollback, groups and a reproducibility lockfile, version pins,
//! self-update and a TUI.

pub mod aqua;
pub mod backend;
pub mod brew;
pub mod config;
pub mod custom;
pub mod elf;
pub mod github;
pub mod group;
pub mod insight;
pub mod install;
pub mod lock;
pub mod mach_o;
pub mod selfupdate;
pub mod snapshot;
pub mod source;
pub mod state;
pub mod tui;
pub mod upgrade;
pub mod winget;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("network: {0}")]
    Network(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}")]
    Checksum {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("{0}")]
    Archive(#[from] selfupdate::ArchiveError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
