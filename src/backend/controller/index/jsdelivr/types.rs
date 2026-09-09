use thiserror::Error;

// impl errors
#[derive(Error, Debug)]
pub enum FetchJSDelivrFailureError {
    #[error("InvalidPath: the requested path is not allowed")]
    InvalidPath,
}
