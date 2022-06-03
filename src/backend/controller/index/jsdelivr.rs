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
    Json(APIResponse<Value>),
    Raw(String),
}

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
            FetchJSDelivrFailureError::Parse(ref e) => write!(f, "{}", e),
            FetchJSDelivrFailureError::PathCovert => write!(f, "Path is not valid UTF-8"),
            FetchJSDelivrFailureError::ReqwestOperation(ref e) => write!(f, "{}", e),
            FetchJSDelivrFailureError::RequestStatusCheck(ref v) => {
                write!(f, "Request status check failed: {}", v)
            }
        }
    }
}

impl error::Error for FetchJSDelivrFailureError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            FetchJSDelivrFailureError::Parse(ref e) => Some(e),
            FetchJSDelivrFailureError::PathCovert => None,
            FetchJSDelivrFailureError::RequestStatusCheck(ref e) => None,
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

fn convert_url(base: &str, path: PathBuf) -> Result<Url, FetchJSDelivrFailureError> {
    let mut url = Url::parse(base)?;
    let path = match path.into_os_string().into_string() {
        Ok(v) => v,
        Err(_) => return Err(FetchJSDelivrFailureError::PathCovert),
    };
    url.set_path(path.as_str());
    Ok(url)
}

async fn fetch_jsdelivr(path: PathBuf) -> Result<String, FetchJSDelivrFailureError> {
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
        .get(convert_url(mirror, path)?)
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
        return Err(FetchJSDelivrFailureError::RequestStatusCheck(
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
            FetchJSDelivrFailureError::Parse(e) => {
                JSDelivrResponse::Json(fail_with_message(400, None, e.to_string()))
            }
            FetchJSDelivrFailureError::ReqwestOperation(e) => {
                JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
            }
            FetchJSDelivrFailureError::RequestStatusCheck(v) => {
                JSDelivrResponse::Json(fail(v as i64, None))
            }
            FetchJSDelivrFailureError::PathCovert => JSDelivrResponse::Json(fail(400, None)),
        },
    }
}
