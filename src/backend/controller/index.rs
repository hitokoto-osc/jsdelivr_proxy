use super::utils::response::success;
use crate::backend::utils::response::ResponseBase;
use chrono::prelude::{DateTime, Utc};
use rocket::{
    get,
    response::status::Custom,
    serde::json::{serde_json::json, Json, Value},
};
use timeago::Formatter;

#[get("/")]
pub fn index() -> Custom<Json<ResponseBase<Value>>> {
    success(json!([]))
}

#[get("/about")]
pub fn about() -> Custom<Json<ResponseBase<Value>>> {
    let now = Utc::now();
    let formatter = Formatter::new();
    success(json!({
        "program": env!("CARGO_PKG_NAME"),
        "version": format!("v{}", env!("CARGO_PKG_VERSION")),
        "profile": env!("BUILD_PROFILE"),
        "build_information": {
            "commit_hash": env!("COMMIT_HASH"),
            "commit_author": env!("COMMIT_AUTHOR"),
            "commit_date": format!(
                "{} ({})",
                env!("COMMIT_DATE"),
                formatter
                    .convert_chrono(
                        DateTime::parse_from_rfc3339(env!("COMMIT_DATE")).unwrap(),
                        now
                    ),
            ),
            "build_time": format!(
                "{} ({})",
                env!("BUILD_TIME"),
                formatter
                    .convert_chrono(
                        DateTime::parse_from_rfc3339(env!("BUIlD_TIME")).unwrap(),
                        now
                    ),
            ),
            "llvm_version": env!("LLVM_VERSION"),
            "rustc_version": env!("RUSTC_VERSION"),
            "build_platform": env!("BUILD_PLATFORM"),
        },
        "feedback": {
            "Kuertianshi": "i@loli.online",
            "freejishu": "i@freejishu.com",
            "a632079": "a632079@qq.com",
            "ada": "adaxh@qq.com"
        },
        "copyright": "MoeTeam © 2022 All Rights Reserved.",
    }))
}
