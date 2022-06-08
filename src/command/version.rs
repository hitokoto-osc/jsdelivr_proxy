use super::Args;
use chrono::prelude::{DateTime, Utc};
use colored::Colorize;
use timeago::Formatter;

pub fn handle_version(args: &Args) {
    if args.version {
        let now = Utc::now();
        let formatter = Formatter::new();
        println!(
            "{} {} (Official {} Build)

{}
Commit Info: {} by {}
Commit Time: {} ({})
Build  Time: {} ({})
LLVM  Version : {}
Rust  Version : {}
Build Platform: {}

",
            env!("CARGO_PKG_NAME"),
            format!("v{}", env!("CARGO_PKG_VERSION")).yellow(),
            env!("BUILD_PROFILE").yellow(),
            "[Build Information]".bright_black(),
            env!("COMMIT_HASH").green(),
            env!("COMMIT_AUTHOR").blue(),
            env!("COMMIT_DATE").cyan(),
            formatter
                .convert_chrono(
                    DateTime::parse_from_rfc3339(env!("COMMIT_DATE")).unwrap(),
                    now
                )
                .red(),
            env!("BUILD_DATE").cyan(),
            formatter
                .convert_chrono(
                    DateTime::parse_from_rfc3339(env!("BUILD_DATE")).unwrap(),
                    now
                )
                .red(),
            env!("LLVM_VERSION").bright_yellow(),
            env!("RUSTC_VERSION").bright_yellow(),
            env!("BUILD_PLATFORM").bright_yellow(),
        );
        std::process::exit(0);
    }
}
