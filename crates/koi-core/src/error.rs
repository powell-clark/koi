use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("monitor `{name}` exceeded budget: {elapsed_ms}ms > {budget_ms}ms")]
    BudgetExceeded {
        name: &'static str,
        elapsed_ms: u64,
        budget_ms: u64,
    },

    #[error("config: {0}")]
    Config(String),
}
