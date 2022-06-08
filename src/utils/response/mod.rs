use super::time::must_get_timestamp;
use phf::phf_map;
use rocket::{http::Status, response::status::Custom, serde::json::Json};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ResponseBase<T> {
    pub status: i64,
    pub message: String,
    pub data: Option<T>,
    pub ts: u128,
}

pub type APIResponse<T> = Custom<Json<ResponseBase<T>>>;

static ERROR_MESSAGE_MAP: phf::Map<&'static str, &'static str> = phf_map! {
    "400" => "Bad Request",
    "401" => "Unauthorized",
    "403" => "Forbidden",
    "404" => "Not Found",
    "500" => "Server Error",
};

pub fn success<T>(data: T) -> Custom<Json<ResponseBase<T>>> {
    success_with_message(data, "Ok".into())
}

pub fn success_with_message<T>(data: T, message: String) -> Custom<Json<ResponseBase<T>>> {
    Custom(
        Status::Ok,
        Json(ResponseBase {
            status: 200,
            message,
            data: Some(data),
            ts: must_get_timestamp(),
        }),
    )
}

pub fn fail<T>(code: i64, data: Option<T>) -> Custom<Json<ResponseBase<T>>> {
    fail_with_message(code, data, "".into())
}

pub fn fail_with_message<T>(
    code: i64,
    data: Option<T>,
    message: String,
) -> Custom<Json<ResponseBase<T>>> {
    Custom(
        if code > 0 {
            Status::new(code as u16)
        } else {
            Status::Ok
        },
        Json(ResponseBase {
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
        }),
    )
}
