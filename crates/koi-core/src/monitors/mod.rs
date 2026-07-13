//! Concrete monitors. Each implements [`crate::Monitor`].

pub mod backup;
pub mod cache;
pub mod disk;
pub mod docker;
pub mod ghostty;
pub mod ghostty_crash;
pub mod git;
pub mod latency;
pub mod memory;
pub mod model_size;
pub mod network;
pub mod package;
pub mod wezterm;
pub mod wezterm_crash;

pub use backup::BackupMonitor;
pub use cache::CacheMonitor;
pub use disk::DiskMonitor;
pub use docker::DockerMonitor;
pub use ghostty::GhosttyMonitor;
pub use git::GitMonitor;
pub use latency::LatencyMonitor;
pub use memory::MemoryMonitor;
pub use model_size::ModelSizeMonitor;
pub use network::NetworkMonitor;
pub use package::PackageMonitor;
pub use wezterm::{process_family_stats, FamilyStats, WezTermMonitor};
pub use wezterm_crash::DetectedCrash;
