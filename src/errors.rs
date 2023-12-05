use std::{error, fmt};

#[derive(Debug)]
#[allow(dead_code)]
pub enum WorkerError {
    Io(std::io::Error),
    Any(String),
}

impl fmt::Display for WorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Any(e) => write!(f, "{e:?}"),
        }
    }
}

impl error::Error for WorkerError {}
