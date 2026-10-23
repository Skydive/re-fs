//! The tablet's flat xochitl directory and the folder tree rebuilt from it.
//! See docs/format.md for the layout this models.

mod entry;
pub mod ino;
#[cfg(feature = "json")]
pub mod naming;
#[cfg(feature = "json")]
pub mod parse;
#[cfg(feature = "json")]
pub mod plan;
#[cfg(feature = "json")]
pub mod tree;

pub use entry::{Entry, FileKind, FileStat};
