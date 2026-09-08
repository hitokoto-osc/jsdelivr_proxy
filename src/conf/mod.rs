use std::error::Error;

use config::{Config as conf, Environment as Env, File, Map};
use serde::Deserialize;

pub mod allowlist;
pub mod cache;
pub mod env;
pub mod jsdelivr;
pub mod server;
use cache::Cache;
use env::Environment;
use jsdelivr::Jsdelivr;

/// 需要按逗号拆成列表的配置项。
///
/// `config` 的环境变量 source 默认把值当标量处理，`Vec<String>` 字段会直接
/// 反序列化失败，因此必须逐个登记；`with_list_parse_key` 只对登记过的键启用
/// 逗号拆分，其余键（如 `jsdelivr.mirror`）仍是普通字符串。
/// 键名是**前缀剥离、分隔符归一成 `.` 之后**的形式，所以单/双下划线两路都适用。
const LIST_VALUED_KEYS: [&str; 3] = [
    "jsdelivr.allowlist.providers",
    "jsdelivr.allowlist.npm",
    "jsdelivr.allowlist.gh",
];

fn with_list_keys(env: Env) -> Env {
    LIST_VALUED_KEYS
        .iter()
        .fold(env.try_parsing(true).list_separator(","), |env, key| {
            env.with_list_parse_key(key)
        })
}

#[derive(Deserialize)]
pub struct Config {
    pub env: Environment,
    #[serde(default)]
    pub cache: Cache,
    #[serde(default)]
    pub jsdelivr: Jsdelivr,
    #[serde(default)]
    pub server: server::Server,
}

impl Config {
    pub fn new(config_path: Option<String>, is_dev: bool) -> Result<Self, Box<dyn Error>> {
        let env: String = if is_dev {
            "Development".to_string()
        } else {
            let run_env = std::env::var("RUN_ENV").unwrap_or_else(|_| "Development".into());
            if run_env == "Testing" {
                "Testing".to_string()
            } else {
                "Production".to_string()
            }
        };
        super::logger::init(env == "Development")?; // 初始化 Logger
        let mut builder = conf::builder().set_override("env", env)?; // 初始化运行环境
        builder = if let Some(path) = config_path {
            builder.add_source(File::with_name(&path).required(true))
        } else {
            // 环境变量覆盖分两路：
            //
            // * 单下划线分隔层级，兼容历史用法（`JSDRLIVR_PROXY_SERVER_PORT`
            //   => `server.port`）；
            // * 双下划线分隔层级，用于字段名自身含下划线的配置项
            //   （`JSDRLIVR_PROXY_CACHE__TTL_SECS` => `cache.ttl_secs`）。
            //   只用单下划线的话 `TTL_SECS` 会被拆成 `ttl.secs` 两层而静默失效。
            //
            // 两个 source 各自只看自己那一半环境变量，避免同一个变量被两种
            // 规则重复解析。
            let (nested, flat): (Map<String, String>, Map<String, String>) =
                std::env::vars().partition(|(key, _)| key.contains("__"));
            builder
                .add_source(File::with_name("conf/config").required(false))
                .add_source(File::with_name("config/config").required(false))
                .add_source(File::with_name("data/config").required(false))
                .add_source(File::with_name("config").required(false))
                .add_source(File::with_name("../conf/config").required(false))
                .add_source(File::with_name("../config").required(false))
                .add_source(
                    with_list_keys(Env::with_prefix("JSDRLIVR_PROXY").separator("_"))
                        .source(Some(flat)),
                )
                .add_source(
                    with_list_keys(
                        Env::with_prefix("JSDRLIVR_PROXY")
                            .prefix_separator("_")
                            .separator("__"),
                    )
                    .source(Some(nested)),
                )
        }; // 交回所有权
        let settings = builder.build()?.try_deserialize::<Self>()?;
        Ok(settings)
    }
}
