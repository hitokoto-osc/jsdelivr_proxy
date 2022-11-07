pub mod types;
use bytes::Bytes;
use deadpool_redis::{redis::AsyncCommands, Connection};
use reqwest::{Client, Url};
use rocket::{get, http::ContentType, serde::json::Value, Responder};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};
use tracing::{error, instrument};

use crate::utils::response::{fail, fail_with_message, APIResponse};
use crate::{
    cache,
    CONFIG,
};

use self::types::FetchJSDelivrFailureError;

#[derive(Responder)]
pub enum JSDelivrResponse {
    Json(APIResponse<Value>),
    Raw(Box<(ContentType, Vec<u8>)>),
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

async fn fetch_jsdelivr(
    path: PathBuf,
) -> Result<(String, Bytes), types::FetchJSDelivrFailureError> {
    let client = Client::builder()
        .user_agent(match &CONFIG.jsdelivr.user_agent {
            Some(v) => v,
            None => concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        })
        .build()?;
    let mirror = match &CONFIG.jsdelivr.mirror {
        Some(v) => v,
        None => "https://cdn.jsdelivr.net",
    };
    let response = client
        .get(convert_url(mirror, path)?)
        .header(
            "Referer",
            match &CONFIG.jsdelivr.referer {
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
    // 由于只使用 GET 方法获取 JSDelivr CDN 的资源，因此 Content-Type 应该就是 Mime
    let mime: String = if let Some(value) = response.headers().get(reqwest::header::CONTENT_TYPE) {
        value.to_str()?.to_string()
    } else {
        "text/plain".to_string()
    };
    Ok((mime, response.bytes().await?))
}

async fn remember_jsdelivr_resource(
    path: PathBuf,
) -> Result<(String, Bytes), FetchJSDelivrFailureError> {
    let key: &[u8] = &Sha256::digest(path.to_string_lossy().to_string().as_bytes());
    let key: String = base16ct::lower::encode_string(key);

    let conn: &mut Connection = &mut (cache::get_connection().await?);
    let mime: Option<String> = conn.get(format!("{}_mime", key)).await?;
    let data: Option<Bytes> = conn.get(format!("{}_data", key)).await?;
    if let (Some(mime), Some(data)) = (mime, data) {
        return Ok((mime, data));
    }
    let (mime, data) = fetch_jsdelivr(path).await?;
    // 保存到 Redis
    conn
        .set_ex(format!("{}_mime", key), mime.clone(), 60 * 60 * 2)
        .await?;
    conn
        .set_ex(format!("{}_data", key), data.to_vec(), 60 * 60 * 2)
        .await?;
    Ok((mime, data))
}

#[get("/<path..>")]
#[instrument]
pub async fn get(path: PathBuf) -> JSDelivrResponse {
    match remember_jsdelivr_resource(path).await {
        Ok((mime, data)) => {
            let content_type = ContentType::from_str(mime.as_str()).unwrap_or(ContentType::Plain);
            JSDelivrResponse::Raw(Box::new((content_type, data.to_vec())))
        }
        Err(ref e) => {
            error!("{:?}", e);
            match e {
                types::FetchJSDelivrFailureError::ReqwestOperation(_) => {
                    JSDelivrResponse::Json(fail_with_message(500, None, e.to_string()))
                }
                types::FetchJSDelivrFailureError::RequestStatusCheck(status) => {
                    JSDelivrResponse::Json(fail(*status as i64, None))
                }
                _ => JSDelivrResponse::Json(fail_with_message(400, None, e.to_string())),
            }
        }
    }
}
