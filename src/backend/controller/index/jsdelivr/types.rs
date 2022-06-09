use std::{
    error,
    fmt::{self, Formatter},
};
use url::ParseError;

use crate::cache::CacheError;

// impl errors
#[derive(Debug)]
pub enum FetchJSDelivrFailureError {
    Parse(ParseError),
    PathCovert,
    ReqwestOperation(reqwest::Error),
    RequestStatusCheck(u16),
}

impl fmt::Display for FetchJSDelivrFailureError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            FetchJSDelivrFailureError::Parse(ref e) => write!(f, "FetchJSDelivrFailureError::Parse: {}", e),
            FetchJSDelivrFailureError::PathCovert => write!(f, "FetchJSDelivrFailureError::PathCovert: Path is not valid UTF-8"),
            FetchJSDelivrFailureError::ReqwestOperation(ref e) => write!(f, "FetchJSDelivrFailureError::ReqwestOperation: {}", e),
            FetchJSDelivrFailureError::RequestStatusCheck(ref v) => {
                write!(f, "FetchJSDelivrFailureError::RequestStatusCheck: Request status check failed: {}", v)
            }
        }
    }
}

impl error::Error for FetchJSDelivrFailureError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            FetchJSDelivrFailureError::Parse(ref e) => Some(e),
            FetchJSDelivrFailureError::PathCovert => None,
            FetchJSDelivrFailureError::RequestStatusCheck(_) => None,
            FetchJSDelivrFailureError::ReqwestOperation(ref e) => Some(e),
        }
    }
}

impl From<ParseError> for FetchJSDelivrFailureError {
    fn from(e: ParseError) -> Self {
        FetchJSDelivrFailureError::Parse(e)
    }
}

impl From<reqwest::Error> for FetchJSDelivrFailureError {
    fn from(e: reqwest::Error) -> Self {
        FetchJSDelivrFailureError::ReqwestOperation(e)
    }
}

// impl Cache

impl fmt::Display for CacheError<FetchJSDelivrFailureError> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            CacheError::Pool(ref e) => write!(f, "CacheError::Pool: {}", e),
            CacheError::Redis(ref e) => write!(f, "CacheError::Pool: {}", e),
            CacheError::RememberFuncCall(ref e) => write!(f, "CacheError::RememberFuncCall: {}", e),
        }
    }
}

impl error::Error for CacheError<FetchJSDelivrFailureError> {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            CacheError::Pool(ref e) => Some(e),
            CacheError::Redis(ref e) => Some(e),
            CacheError::RememberFuncCall(ref e) => Some(e),
        }
    }
}
