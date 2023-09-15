use thiserror::Error;
use url::ParseError;

// impl errors
#[derive(Error, Debug)]
pub enum FetchJSDelivrFailureError {
    #[error("FetchJSDelivrFailureError::PathCovert: Path is not valid UTF-8")]
    Parse(#[from] ParseError),
    #[error("Failed to fetch from JSDelivr")]
    PathCovert,
    #[error("ReqwestOperation failed: {0}")]
    ReqwestOperation(#[from] reqwest::Error),
    #[error("RequestStatusCheck failed: {0}")]
    RequestStatusCheck(u16),
    #[error("RequestContentTypeConvert: {0}")]
    RequestContentTypeConvert(#[from] reqwest::header::ToStrError),
    #[error("CacheError::Pool: {0}")]
    RedisPool(#[from] deadpool_redis::PoolError),
    #[error("CacheError::Redis: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),
}

/*
impl fmt::Display for FetchJSDelivrFailureError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            FetchJSDelivrFailureError::Parse(ref e) => write!(f, "FetchJSDelivrFailureError::Parse: {}", e),
            FetchJSDelivrFailureError::PathCovert => write!(f, "FetchJSDelivrFailureError::PathCovert: Path is not valid UTF-8"),
            FetchJSDelivrFailureError::ReqwestOperation(ref e) => write!(f, "FetchJSDelivrFailureError::ReqwestOperation: {}", e),
            FetchJSDelivrFailureError::RequestStatusCheck(ref v) => {
                write!(f, "FetchJSDelivrFailureError::RequestStatusCheck: Request status check failed: {}", v)
            },
            FetchJSDelivrFailureError::RequestContentTypeConvert(ref e) => {
                write!(f, "FetchJSDelivrFailureError::RequestContentTypeConvert: {}", e)
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
            FetchJSDelivrFailureError::RequestContentTypeConvert(ref e) => Some(e),
        }
    }
}
 */

/*
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

impl From<reqwest::header::ToStrError> for FetchJSDelivrFailureError {
    fn from(e: reqwest::header::ToStrError) -> Self {
        FetchJSDelivrFailureError::RequestContentTypeConvert(e)
    }
}
 */
