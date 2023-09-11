use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("error serializing data: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("invalid HTTP response status code {0}")]
    InvalidHTTPStatusCodeError(u16),
    #[error("GraphQL error")]
    ResponseError(#[from] ResponseError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// An error response, consisting of a status code and an optional list of errors
#[derive(Deserialize, Serialize, Debug, thiserror::Error)]
pub struct ResponseError {
    pub status_code: u16,
    pub code: Option<String>,
    pub errors: Vec<GraphQLError>,
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "status code {}", self.status_code)?;
        if !self.errors.is_empty() {
            write!(f, ": ")?;
        }
        for (ii, error) in self.errors.iter().enumerate() {
            if ii > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", error)?;
        }
        Ok(())
    }
}

/// A collection of GraphQL errors as returned from the server
#[derive(Deserialize, Serialize, Debug)]
pub struct GraphQLErrors {
    pub code: Option<String>,
    pub errors: Vec<GraphQLError>,
}

/// A single GraphQL error with a message
#[derive(Deserialize, Serialize, Debug)]
pub struct GraphQLError {
    pub message: String,
}

impl fmt::Display for GraphQLError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
