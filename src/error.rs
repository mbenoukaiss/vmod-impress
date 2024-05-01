use std::fmt;

#[derive(Debug)]
pub enum Error {
    Other(String),
}

impl Error {
    pub fn new<S>(msg: S) -> Error where S: ToString {
        Error::Other(msg.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match self {
            Error::Other(s) => write!(f, "{}", s),
        }
    }
}

impl<T: std::error::Error> From<T> for Error {
    fn from(err: T) -> Self {
        Error::new(err.to_string())
    }
}
