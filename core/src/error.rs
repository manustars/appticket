//! Errors of the domain operations, independent of how they are reached (HTTP, command line).

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The named thing does not exist.
    NotFound(&'static str),
    /// The operation would break a rule of docs/domain-rules.md; the text says which.
    Rule(String),
    /// The agent's role does not allow it; the text says what.
    Forbidden(String),
    Internal(anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn rule<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::Rule(why.into()))
}

impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(error: E) -> Self {
        Error::Internal(error.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(what) => write!(f, "{what} not found"),
            Error::Rule(why) | Error::Forbidden(why) => f.write_str(why),
            Error::Internal(error) => write!(f, "{error:#}"),
        }
    }
}
