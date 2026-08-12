//! koi-core — platform-agnostic domain logic.
//!
//! See ADR-0013 (Rust + Tauri v2 architecture). No I/O orchestration here;
//! daemons and CLIs compose these primitives.

pub mod backup_convergence;
pub mod cleaners;
pub mod config;
pub mod error;
pub mod filing;
pub mod fs_size;
pub mod monitor;
pub mod monitors;
pub mod notes;
pub mod sensors;
pub mod state;
pub mod types;

pub use error::{Error, Result};
pub use monitor::Monitor;
pub use types::{HealthStatus, MonitorReport, Severity};
