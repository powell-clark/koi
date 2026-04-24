use crate::{types::MonitorReport, Result};

/// Platform-agnostic monitor contract.
///
/// Implementations must be cheap to call (see ADR-0013: <200ms combined budget
/// for health monitors). Honour `budget_ms()` — the daemon enforces it.
pub trait Monitor: Send + Sync {
    fn name(&self) -> &'static str;

    fn budget_ms(&self) -> u64 {
        200
    }

    fn run(&self) -> Result<MonitorReport>;
}
