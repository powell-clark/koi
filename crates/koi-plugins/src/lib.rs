//! koi-plugins — plugin host for user-defined rules and filters.
//!
//! Scope per ADR-0013: expose a stable host API for WASM (heavy) and Rhai
//! (lightweight scripting) extensions. Concrete runtime integration is deferred
//! to a dedicated story; this crate currently publishes the host-API shape.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub runtime: PluginRuntime,
    pub entry: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginRuntime {
    Rhai,
    Wasm,
}
