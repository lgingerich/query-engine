use thiserror::Error;

/// Errors from turning a bound query into a scan plan
#[derive(Debug, Error)]
pub(crate) enum PlanError {
    /// Row group footer reports a negative row count
    #[error("row group {row_group}: invalid row count {num_rows}")]
    InvalidRowCount {
        /// Row group index
        row_group: usize,
        /// Row count from the footer
        num_rows: i64,
    },
}
