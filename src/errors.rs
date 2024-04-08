use std::{error, fmt};

#[derive(Debug)]
#[allow(dead_code)]
pub enum SageError {
    Io(std::io::Error),
    Generic(String),
}

impl SageError {
    pub fn to_generic<E: error::Error>(error: E) -> Self {
        Self::Generic(error.to_string())
    }
}

impl fmt::Display for SageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{}", e),
            Self::Generic(e) => write!(f, "{}", e),
        }
    }
}

impl error::Error for SageError {}

impl From<std::io::Error> for SageError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
