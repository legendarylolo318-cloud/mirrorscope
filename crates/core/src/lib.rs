//! Core logic for mirrorscope: fetching mirror status, filtering, sorting,
//! speed testing and mirrorlist formatting. No UI code lives here.

pub mod backups;
pub mod config;
pub mod error;
pub mod fetch;
pub mod filter;
pub mod mirrorlist;
pub mod model;
pub mod privileged;
pub mod select;
pub mod sort;
pub mod speedtest;
pub mod units;

pub use error::{Error, Result};
pub use filter::{CompiledFilter, FilterConfig};
pub use model::{Mirror, Protocol, StatusDocument};
pub use sort::{SortKey, SortOrder};

/// Status JSON endpoint used by `reflector` and the Arch website.
pub const STATUS_URL: &str = "https://archlinux.org/mirrors/status/json/";
/// Path (relative to a mirror root) downloaded for speed measurement.
pub const SPEEDTEST_PATH: &str = "core/os/x86_64/core.db";
/// System mirrorlist location.
pub const SYSTEM_MIRRORLIST: &str = "/etc/pacman.d/mirrorlist";
/// Directory holding the system mirrorlist and its backups.
pub const PACMAN_DIR: &str = "/etc/pacman.d";
