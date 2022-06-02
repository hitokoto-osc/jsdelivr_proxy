use std::error::Error;

use config::{Config as conf, Environment as Env, File};
use serde::Deserialize;

mod database;
mod env;
mod rabbitmq;
mod redis;
use self::redis::Redis;
use database::Database;
use env::Environment;
use rabbitmq::RabbitMQ;
#[derive(Deserialize)]
pub struct Config {
    pub database: Database,
    pub redis: Redis,
    pub rabbitmq: RabbitMQ,
    pub env: Environment,
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
        let mut builder = conf::builder()
            .set_override("env", env)? // 初始化运行环境
            .add_source(File::with_name("conf/config.toml").required(false))
            .add_source(File::with_name("config.toml").required(false))
            .add_source(File::with_name("../conf/config.toml").required(false))
            .add_source(File::with_name("../config.toml").required(false))
            .add_source(Env::with_prefix("HITOKOTO_POLL"));
        builder = if let Some(path) = config_path {
            builder.add_source(File::with_name(&path))
        } else {
            builder
        }; // 交回所有权
        let settings = builder.build()?.try_deserialize::<Self>()?;
        Ok(settings)
    }
}
