//! Error types for pdast.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("No root canvas found in patch")]
    NoRootCanvas,

    #[error("Unexpected end of canvas stack")]
    StackUnderflow,

    #[error("{0}")]
    Other(String),
}
