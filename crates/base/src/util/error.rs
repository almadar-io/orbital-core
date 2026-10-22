//! Error types for Orbital core operations

use thiserror::Error;

/// Main error type for Orbital operations
#[derive(Error, Debug)]
pub enum OrbitalError {
    #[error("Schema parse error: {0}")]
    SchemaParse(#[from] serde_json::Error),

    #[error("Schema parse error at {0}")]
    SchemaParsePath(#[from] serde_path_to_error::Error<serde_json::Error>),

    #[error("Schema validation error: {0}")]
    SchemaValidation(String),

    #[error("Evaluation error: {0}")]
    Eval(#[from] EvalError),

    #[error("State machine error: {0}")]
    StateMachine(String),

    #[error("Effect execution error: {0}")]
    EffectExecution(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Evaluation-specific errors
#[derive(Error, Debug)]
pub enum EvalError {
    #[error("Unknown binding: {0}")]
    UnknownBinding(String),

    #[error("Unknown operator: {0}")]
    UnknownOperator(String),

    #[error("Type mismatch: expected {expected}, got {actual}")]
    TypeMismatch { expected: String, actual: String },

    #[error("Arity mismatch for '{operator}': expected {expected} args, got {actual}")]
    ArityMismatch {
        operator: String,
        expected: usize,
        actual: usize,
    },

    #[error("Division by zero")]
    DivisionByZero,

    #[error("Index out of bounds: {index} (length: {length})")]
    IndexOutOfBounds { index: usize, length: usize },

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
}

pub type Result<T> = std::result::Result<T, OrbitalError>;
