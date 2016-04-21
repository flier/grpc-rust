use std::io;
use std::fmt;
use std::str;
use std::num;
use std::error::Error;

use solicit::http;
use openssl::ssl;

#[derive(Debug)]
pub enum GrpcError {
    Utf8Error(str::Utf8Error),
    ParseIntError(num::ParseIntError),
    IoError(io::Error),
    HttpError(http::HttpError),
    SslError(ssl::error::SslError),
    BadRequest(&'static str),
}

impl fmt::Display for GrpcError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &GrpcError::Utf8Error(ref err) => write!(f, "parse utf8 error, {}", err.description()),
            &GrpcError::ParseIntError(ref err) => {
                write!(f, "parse int error, {}", err.description())
            }
            &GrpcError::IoError(ref err) => write!(f, "io error, {}", err.description()),
            &GrpcError::HttpError(ref err) => write!(f, "http error, {}", err.description()),
            &GrpcError::SslError(ref err) => write!(f, "ssl error, {}", err.description()),
            &GrpcError::BadRequest(ref err) => write!(f, "bad request, {}", err),
        }
    }
}

impl Error for GrpcError {
    fn description(&self) -> &str {
        match self {
            &GrpcError::Utf8Error(ref err) => err.description(),
            &GrpcError::ParseIntError(ref err) => err.description(),
            &GrpcError::IoError(ref err) => err.description(),
            &GrpcError::HttpError(ref err) => err.description(),
            &GrpcError::SslError(ref err) => err.description(),
            &GrpcError::BadRequest(ref err) => err,
        }
    }
}

impl From<str::Utf8Error> for GrpcError {
    fn from(err: str::Utf8Error) -> Self {
        GrpcError::Utf8Error(err)
    }
}

impl From<num::ParseIntError> for GrpcError {
    fn from(err: num::ParseIntError) -> Self {
        GrpcError::ParseIntError(err)
    }
}

impl From<io::Error> for GrpcError {
    fn from(err: io::Error) -> Self {
        GrpcError::IoError(err)
    }
}

impl From<http::HttpError> for GrpcError {
    fn from(err: http::HttpError) -> Self {
        GrpcError::HttpError(err)
    }
}

impl From<ssl::error::SslError> for GrpcError {
    fn from(err: ssl::error::SslError) -> Self {
        GrpcError::SslError(err)
    }
}

pub type GrpcResult<T> = Result<T, GrpcError>;
