use super::time::must_get_timestamp;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use phf::phf_map;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ResponseBase<T> {
    pub status: i64,
    pub message: String,
    pub data: Option<T>,
    pub ts: u128,
}

/// 统一的 API 响应：HTTP 状态码 + JSON 响应体
pub struct APIResponse<T>(pub StatusCode, pub ResponseBase<T>);

impl<T> IntoResponse for APIResponse<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

static ERROR_MESSAGE_MAP: phf::Map<&'static str, &'static str> = phf_map! {
    "400" => "Bad Request",
    "401" => "Unauthorized",
    "403" => "Forbidden",
    "404" => "Not Found",
    "500" => "Server Error",
};

/// 将业务状态码转换为 HTTP 状态码，保持与 Rocket 版本一致的语义：
/// code > 0 时使用 code 本身，否则回落到 200。
fn resolve_status(code: i64) -> StatusCode {
    if code > 0 {
        StatusCode::from_u16(code as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    } else {
        StatusCode::OK
    }
}

pub fn success<T>(data: T) -> APIResponse<T> {
    success_with_message(data, "Ok".into())
}

pub fn success_with_message<T>(data: T, message: String) -> APIResponse<T> {
    APIResponse(
        StatusCode::OK,
        ResponseBase {
            status: 200,
            message,
            data: Some(data),
            ts: must_get_timestamp(),
        },
    )
}

pub fn fail<T>(code: i64, data: Option<T>) -> APIResponse<T> {
    fail_with_message(code, data, "".into())
}

pub fn fail_with_message<T>(code: i64, data: Option<T>, message: String) -> APIResponse<T> {
    APIResponse(
        resolve_status(code),
        ResponseBase {
            status: code,
            message: if message.is_empty() {
                match ERROR_MESSAGE_MAP.get(&code.to_string()).cloned() {
                    Some(v) => v.to_string(),
                    None => "Unknown Error. Please contact developer.".into(),
                }
            } else {
                message
            },
            data,
            ts: must_get_timestamp(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn success_uses_200() {
        let resp = success(Value::Null);
        assert_eq!(resp.0, StatusCode::OK);
        assert_eq!(resp.1.status, 200);
        assert_eq!(resp.1.message, "Ok");
    }

    #[test]
    fn fail_maps_known_codes_to_messages() {
        let resp: APIResponse<Value> = fail(404, None);
        assert_eq!(resp.0, StatusCode::NOT_FOUND);
        assert_eq!(resp.1.status, 404);
        assert_eq!(resp.1.message, "Not Found");
    }

    #[test]
    fn fail_with_unknown_code_falls_back_to_generic_message() {
        let resp: APIResponse<Value> = fail(418, None);
        assert_eq!(resp.0, StatusCode::IM_A_TEAPOT);
        assert_eq!(resp.1.message, "Unknown Error. Please contact developer.");
    }

    #[test]
    fn non_positive_code_stays_200() {
        let resp: APIResponse<Value> = fail(0, None);
        assert_eq!(resp.0, StatusCode::OK);
        assert_eq!(resp.1.status, 0);
    }
}
