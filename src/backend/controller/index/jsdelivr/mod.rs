pub mod types;
use std::path::{Path, PathBuf};

use reqwest::{Client, Url};
use rocket::{get, serde::json::Value, Responder};
use sha2::{Digest, Sha256};
use tracing::{error, instrument};

use crate::utils::response::{fail, fail_with_message, APIResponse};
use crate::{
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
    let mut path = match path.into_os_string().into_string() {
        Ok(v) => v,
        Err(_) => return Err(types::FetchJSDelivrFailureError::PathCovert),
    };
    let raw_path = url.path();
    if raw_path != "/" {
        path = match Path::new("/")
            .join(raw_path)
            .join(path)
            .into_os_string()
            .into_string()
        {
            Ok(v) => v,
            Err(_) => return Err(types::FetchJSDelivrFailureError::PathCovert),
        };
    }
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
#[instrument]
pub async fn get(path: PathBuf) -> JSDelivrResponse {
    let key: &[u8] = &Sha256::digest(path.to_string_lossy().to_string().as_bytes());
    let key: String = base16ct::lower::encode_string(key);
    let res: Result<String, CacheError<types::FetchJSDelivrFailureError>> = cache::remember(
        key,
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
        Err(ref e) => {
            error!("{:?}", e);
            match e {
                CacheError::RememberFuncCall(v) => match &v.0 {
                    types::FetchJSDelivrFailureError::ReqwestOperation(_) => {
                        JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
                    }
                    types::FetchJSDelivrFailureError::RequestStatusCheck(status) => {
                        JSDelivrResponse::Json(fail(*status as i64, None))
                    }
                    _ => JSDelivrResponse::Json(fail_with_message(400, None, e.to_string())),
                },
                _ => JSDelivrResponse::Json(fail_with_message(500, None, e.to_string())),
            }
        }
    }
}
