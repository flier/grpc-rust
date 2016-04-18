use std::io;
use std::fmt;
use std::convert;
use std::error::Error;

use openssl::ssl;

#[derive(Debug)]
pub enum GrpcError {
    IoError(io::Error),
    SslError(ssl::error::SslError),
}

impl fmt::Display for GrpcError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &GrpcError::IoError(ref err) => write!(f, "io error, {}", err.description()),
            &GrpcError::SslError(ref err) => write!(f, "ssl error, {}", err.description()),
        }
    }
}

impl Error for GrpcError {
    fn description(&self) -> &str {
        match self {
            &GrpcError::IoError(ref err) => err.description(),
            &GrpcError::SslError(ref err) => err.description(),
        }
    }
}

impl From<io::Error> for GrpcError {
    fn from(err: io::Error) -> Self {
        GrpcError::IoError(err)
    }
}

impl From<ssl::error::SslError> for GrpcError {
    fn from(err: ssl::error::SslError) -> Self {
        GrpcError::SslError(err)
    }
}

pub type GrpcResult<T> = Result<T, GrpcError>;
