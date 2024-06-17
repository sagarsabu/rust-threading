use std::{error, fmt};

#[derive(Debug)]
pub enum ErrorWrap {
    Io(std::io::Error),
    Generic(String),
}

impl ErrorWrap {
    pub fn to_generic<E: error::Error>(error: E) -> Self {
        Self::Generic(error.to_string())
    }
}

impl fmt::Display for ErrorWrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{}", e),
            Self::Generic(e) => write!(f, "{}", e),
        }
    }
}

impl error::Error for ErrorWrap {}

impl From<std::io::Error> for ErrorWrap {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<&str> for ErrorWrap {
    fn from(value: &str) -> Self {
        Self::Generic(value.to_string())
    }
}

impl From<String> for ErrorWrap {
    fn from(value: String) -> Self {
        Self::Generic(value.to_string())
    }
}
