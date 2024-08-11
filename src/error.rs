use std::error::Error as StdError;
use std::fmt;
use std::sync::mpsc::SendError;
use std::sync::PoisonError;

#[derive(Debug)]
pub enum Error {
    Custom(String),
    Poison(String),
    Send(String),
    Other(Box<dyn StdError>),
}

impl Error {
    pub fn new<S>(msg: S) -> Error
    where
        S: ToString,
    {
        Error::Custom(msg.to_string())
    }

    pub fn err<S, T>(msg: S) -> Result<T, Error>
    where
        S: ToString,
    {
        Err(Error::Custom(msg.to_string()))
    }
}

impl StdError for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match self {
            Error::Custom(s) => write!(f, "{}", s),
            Error::Poison(s) => write!(f, "{}", s),
            Error::Send(s) => write!(f, "{}", s),
            Error::Other(s) => write!(f, "{}", s),
        }
    }
}

macro_rules! error_from {
    ($instance:path, $strct:ty) => {
        impl From<$strct> for Error {
            fn from(err: $strct) -> Self {
                $instance(err)
            }
        }
    };
    ($instance:path, bx $strct:ty) => {
        impl From<$strct> for Error {
            fn from(err: $strct) -> Self {
                $instance(Box::new(err))
            }
        }
    };
    ($instance:path, bx $strct:ty, $gen:ident) => {
        impl<'a, $gen: 'a> From<$strct> for Error {
            fn from(err: $strct) -> Self {
                $instance(Box::new(err))
            }
        }
    };
}

impl<T> From<PoisonError<T>> for Error {
    fn from(value: PoisonError<T>) -> Self {
        Error::Poison(value.to_string())
    }
}

impl<T> From<SendError<T>> for Error {
    fn from(value: SendError<T>) -> Self {
        Error::Send(value.to_string())
    }
}

error_from!(Error::Other, bx ron::error::SpannedError);
error_from!(Error::Other, bx std::string::FromUtf8Error);
error_from!(Error::Other, bx regex::Error);
error_from!(Error::Other, bx libavif::Error);
error_from!(Error::Other, bx turbojpeg::Error);
error_from!(Error::Other, bx image::ImageError);
error_from!(Error::Other, bx std::io::Error);
error_from!(Error::Other, bx varnish::vcl::Error);

pub trait MapResultString<T> {
    fn or_display<S>(self, msg: S) -> Result<T, Error>
    where
        S: ToString;
}

impl<T, E> MapResultString<T> for Result<T, E> {
    fn or_display<S>(self, msg: S) -> Result<T, Error>
    where
        S: ToString,
    {
        self.map_err(|_| Error::new(msg))
    }
}
