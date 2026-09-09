use colored::*;
use tracing::{info, warn};
mod audit;
pub mod backend;
mod cache;
mod command;
pub mod conf;
mod logger;
mod metrics;
mod preload;
mod upstream;
pub mod utils;

#[macro_use]
extern crate lazy_static;
lazy_static! {
    pub static ref CONFIG: conf::Config = {
        let args = command::handle_args().expect("Failed to handle command line arguments"); // This is a trick to parse commands before config
        conf::Config::new(args.config_path, args.dev).expect("Failed to load config")
    };
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env = &CONFIG.env; // 获取运行环境，同时读取 CONFIG 读取逻辑
    info!(
        "You are running {}(v{}) in {} mode.",
        env!("CARGO_PKG_NAME").bright_black(),
        env!("CARGO_PKG_VERSION").red(),
        format!("{}", env).blue().bold()
    );

    info!(
        "Cache: in-process, ttl={}s, capacity={}MB, max entry={}MB, compression={} (no external dependency)",
        CONFIG.cache.ttl_secs,
        CONFIG.cache.max_capacity_mb,
        CONFIG.cache.max_entry_size_mb,
        CONFIG.cache.compression
    );

    #[allow(clippy::eq_op)]
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
    // The webhook also writes audit records, so the log has to exist whenever
    // either entry point is on.
    if CONFIG.admin.is_enabled() || CONFIG.admin.is_webhook_enabled() {
        audit::init().await;
    }
    if CONFIG.admin.is_enabled() {
        metrics::spawn();
        info!("Admin panel and API: enabled at /admin");
    } else {
        info!("Admin panel and API: disabled (no admin key configured)");
    }
    info!(
        "Cache purge webhook: {}",
        if CONFIG.admin.is_webhook_enabled() {
            "enabled at POST /webhook/cache/purge"
        } else {
            "disabled (no webhook secret configured)"
        }
    );

    preload::spawn(); // background task; must not delay startup

    info!("Starting HTTP Server...");
    backend::init().await?; // 启动 axum Web Server
    Ok(())
}
