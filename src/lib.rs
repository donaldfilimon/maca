// Maca: cross-platform (macOS/Linux) package manager.
// Native catalog APIs for Homebrew + winget, plus a custom registry backend.
// No `brew`/`winget` subprocesses are required for search/catalog; installs
// download and verify artifacts directly.

pub mod backend;
pub mod brew;
pub mod config;
pub mod custom;
pub mod install;
pub mod mach_o;
pub mod state;
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
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
