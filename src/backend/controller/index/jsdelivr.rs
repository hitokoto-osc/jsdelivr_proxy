use std::{error, fmt, path::PathBuf};

use reqwest::{Client, Url};
use rocket::{get, serde::json::Value, Responder};
use url::ParseError;

use crate::{
    backend::utils::response::{fail, fail_with_message, APIResponse},
    CONFIG,
};

#[derive(Responder)]
pub enum JSDelivrResponse {
    JSON(APIResponse<Value>),
    Raw(String),
}

// impl errors
#[derive(Debug)]
pub enum FetchJSDelivrError {
    ParseFailed(ParseError),
    PathCovertFailed,
    ReqwestOperationFailed(reqwest::Error),
    RequestStatusCheckFailed(u16),
}

impl fmt::Display for FetchJSDelivrError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            FetchJSDelivrError::ParseFailed(ref e) => write!(f, "{}", e.to_string()),
            FetchJSDelivrError::PathCovertFailed => write!(f, "Path is not valid UTF-8"),
            FetchJSDelivrError::ReqwestOperationFailed(ref e) => write!(f, "{}", e.to_string()),
            FetchJSDelivrError::RequestStatusCheckFailed(ref v) => {
                write!(f, "Request status check failed: {}", v)
            }
        }
    }
}

impl error::Error for FetchJSDelivrError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            FetchJSDelivrError::ParseFailed(ref e) => Some(e),
            FetchJSDelivrError::PathCovertFailed => None,
            FetchJSDelivrError::RequestStatusCheckFailed(ref e) => None,
            FetchJSDelivrError::ReqwestOperationFailed(ref e) => Some(e),
        }
    }
}

impl From<ParseError> for FetchJSDelivrError {
    fn from(e: ParseError) -> Self {
        FetchJSDelivrError::ParseFailed(e)
    }
}

impl From<reqwest::Error> for FetchJSDelivrError {
    fn from(e: reqwest::Error) -> Self {
        FetchJSDelivrError::ReqwestOperationFailed(e)
    }
}

fn convert_url(base: &str, path: PathBuf) -> Result<Url, FetchJSDelivrError> {
    let mut url = Url::parse(base)?;
    let path = match path.into_os_string().into_string() {
        Ok(v) => v,
        Err(_) => return Err(FetchJSDelivrError::PathCovertFailed),
    };
    url.set_path(path.as_str());
    Ok(url)
}

async fn fetch_jsdelivr(path: PathBuf) -> Result<String, FetchJSDelivrError> {
    let client = Client::builder()
        .user_agent(match &(*CONFIG).jsdelivr.user_agent {
            Some(v) => v,
            None => concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        })
        .build()?;
    let mirror = match &(*CONFIG).jsdelivr.mirror {
        Some(v) => v,
        None => "https://cdn.jsdelivr.net",
    };
    let response = client
        .get(convert_url(&mirror, path)?)
        .header(
            "Referer",
            match &(*CONFIG).jsdelivr.referer {
                Some(v) => v,
                None => mirror,
            },
        )
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchJSDelivrError::RequestStatusCheckFailed(
            status.as_u16(),
        ));
    }
    Ok(response.text().await?)
}

#[get("/<path..>")]
pub async fn get(path: PathBuf) -> JSDelivrResponse {
    match fetch_jsdelivr(path).await {
        Ok(v) => JSDelivrResponse::Raw(v),
        Err(e) => match e {
            FetchJSDelivrError::ParseFailed(e) => {
                JSDelivrResponse::JSON(fail_with_message(400, None, e.to_string()))
            }
            FetchJSDelivrError::ReqwestOperationFailed(e) => {
                JSDelivrResponse::JSON(fail_with_message(500, None, e.to_string()))
            }
            FetchJSDelivrError::RequestStatusCheckFailed(v) => {
                JSDelivrResponse::JSON(fail(v as i64, None))
            }
            FetchJSDelivrError::PathCovertFailed => JSDelivrResponse::JSON(fail(400, None)),
        },
    }
}
