use thiserror::Error;

use crate::upstream::UpstreamError;

// impl errors
#[derive(Error, Debug)]
pub enum FetchJSDelivrFailureError {
    #[error("InvalidPath: the requested path is not allowed")]
    InvalidPath,
    #[error(transparent)]
    Upstream(#[from] UpstreamError),
}
