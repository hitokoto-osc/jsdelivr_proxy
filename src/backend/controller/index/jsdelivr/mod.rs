pub mod types;
use std::path::PathBuf;

use reqwest::{Client, Url};
use rocket::{get, serde::json::Value, Responder};

use crate::{
    backend::utils::response::{fail, fail_with_message, APIResponse},
    cache::{self, CacheError},
    CONFIG,
};

#[derive(Responder)]
pub enum JSDelivrResponse {
    Json(APIResponse<Value>),
    Raw(String),
}

fn convert_url(base: &str, path: PathBuf) -> Result<Url, types::FetchJSDelivrFailureError> {
    let mut url = Url::parse(base)?;
    let path = match path.into_os_string().into_string() {
        Ok(v) => v,
        Err(_) => return Err(types::FetchJSDelivrFailureError::PathCovert),
    };
    url.set_path(path.as_str());
    Ok(url)
}

async fn fetch_jsdelivr(path: PathBuf) -> Result<String, types::FetchJSDelivrFailureError> {
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
        return Err(types::FetchJSDelivrFailureError::RequestStatusCheck(
            status.as_u16(),
        ));
    }
    Ok(response.text().await?)
}

#[get("/<path..>")]
pub async fn get(path: PathBuf) -> JSDelivrResponse {
    let res: Result<String, CacheError<types::FetchJSDelivrFailureError>> = cache::remember(
        path.to_string_lossy().to_string(),
        || async {
            match fetch_jsdelivr(path).await {
                Ok(v) => Ok(v),
                Err(e) => Err(cache::RememberFuncCallError::from(e)),
            }
        },
        Some(60 * 60 * 2), // 2 hours
    )
    .await;
    match res {
        Ok(v) => JSDelivrResponse::Raw(v),
        Err(ref e) => match e {
            CacheError::Pool(e) => {
                JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
            }
            CacheError::Redis(e) => {
                JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
            }
            CacheError::RememberFuncCall(v) => match &v.0 {
                types::FetchJSDelivrFailureError::Parse(e) => {
                    JSDelivrResponse::Json(fail_with_message(400, None, e.to_string()))
                }
                types::FetchJSDelivrFailureError::PathCovert => JSDelivrResponse::Json(
                    fail_with_message(400, None, "Path is not valid UTF-8".to_string()),
                ),
                types::FetchJSDelivrFailureError::ReqwestOperation(e) => {
                    JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
                }
                types::FetchJSDelivrFailureError::RequestStatusCheck(status) => {
                    JSDelivrResponse::Json(fail(*status as i64, None))
                }
            },
        },
    }
}
