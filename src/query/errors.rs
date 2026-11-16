use thiserror::Error;

#[derive(Debug, Error)]
pub enum QueryError {
    #[error(transparent)]
    Parse(#[from] sqlparser::parser::ParserError),
    #[error("unsupported SQL: {0}")]
    Unsupported(String),
}
