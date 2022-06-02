use std::error::Error;

use colored::*;
use tracing::{info, warn};
mod backend;
mod cache;
mod command;
mod conf;
mod logger;

#[macro_use]
extern crate lazy_static;
lazy_static! {
    pub static ref CONFIG: conf::Config = {
        let args = command::handle_args().expect("Failed to handle command line arguments"); // This is a trick to parse commands before config
        conf::Config::new(args.config_path, args.dev).expect("Failed to load config")
    };
}

#[rocket::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let env = &(*CONFIG).env; // 获取运行环境，同时读取 CONFIG 读取逻辑
    info!(
        "You are running {}(v{}) in {} mode.",
        env!("CARGO_PKG_NAME").bright_black(),
        env!("CARGO_PKG_VERSION").red(),
        format!("{}", env).blue().bold()
    );

    if env!("BUILD_PROFILE") == "Debug" {
        // 测试版本警告
        warn!(
            "{}",
            format!(
                "This program is a {} build version. It might be not stable and optimized. You should be serious to use it, or use a release build version.", 
                env!("BUILD_PROFILE").to_uppercase().bold()
            )
            .yellow()
        );
    }
    info!("Starting HTTP Server...");
    backend::init().await?; // 启动 Rocket Web Server
    Ok(())
}
