use clap::Parser;
use std::error::Error;

mod version;
#[derive(Parser, Debug)]
#[clap(author, about, long_about = None)]
pub struct Args {
    #[clap(short = 'D', long, help = "Run in development mode")]
    pub dev: bool, // 开发模式
    #[clap(short, long, help = "Specify config file path")]
    pub config_path: Option<String>, // 手动指定配置路径
    #[clap(short, long, help = "Print version and build information")]
    pub version: bool, // 显示版本信息
}

/*
pub enum Command {
    Test,
}
*/

pub fn handle_args() -> Result<Args, Box<dyn Error>> {
    let args = Args::parse();
    version::handle_version(&args);

    Ok(args)
}
