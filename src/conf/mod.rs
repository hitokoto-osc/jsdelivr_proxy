use std::error::Error;

use config::{Config as conf, Environment as Env, File};
use serde::Deserialize;

pub mod database;
pub mod env;
pub mod jsdelivr;
pub mod rabbitmq;
pub mod redis;
pub mod server;
use self::redis::Redis;
use database::Database;
use env::Environment;
use jsdelivr::Jsdelivr;
use rabbitmq::RabbitMQ;

#[derive(Deserialize)]
pub struct Config {
    pub env: Environment,
    #[serde(default)]
    pub database: Database,
    #[serde(default)]
    pub jsdelivr: Jsdelivr,
    #[serde(default)]
    pub redis: Redis,
    #[serde(default)]
    pub rabbitmq: RabbitMQ,
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
            builder
                .add_source(File::with_name("conf/config").required(false))
                .add_source(File::with_name("config/config").required(false))
                .add_source(File::with_name("data/config").required(false))
                .add_source(File::with_name("config").required(false))
                .add_source(File::with_name("../conf/config").required(false))
                .add_source(File::with_name("../config").required(false))
                .add_source(
                    Env::with_prefix("JSDRLIVR_PROXY")
                        .try_parsing(true)
                        .separator("_"),
                )
        }; // 交回所有权
        let settings = builder.build()?.try_deserialize::<Self>()?;
        Ok(settings)
    }
}
