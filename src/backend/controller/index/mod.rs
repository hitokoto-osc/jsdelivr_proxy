pub mod gravatar;
pub mod jsdelivr;
use crate::utils::response::success;
use crate::utils::response::APIResponse;
use axum::http::header;
use axum::response::IntoResponse;
use chrono::prelude::{DateTime, Utc};
use serde_json::{json, Value};
use timeago::Formatter;

pub async fn index() -> APIResponse<Value> {
    success(json!([]))
}

pub async fn favicon() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/x-icon")],
        include_bytes!("../../../../assets/images/favicon.ico").as_slice(),
    )
}

pub async fn about() -> APIResponse<Value> {
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
                env!("BUILD_DATE"),
                formatter
                    .convert_chrono(
                        DateTime::parse_from_rfc3339(env!("BUILD_DATE")).unwrap(),
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
