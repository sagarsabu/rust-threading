use std::{error, fmt};

#[derive(Debug)]
#[allow(dead_code)]
pub enum SageError {
    Io(std::io::Error),
    Generic(String),
}

impl fmt::Display for SageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Generic(e) => write!(f, "{e:?}"),
        }
    }
}

impl error::Error for SageError {}
