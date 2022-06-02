use tracing_subscriber::{self, filter::EnvFilter, filter::LevelFilter, fmt::Subscriber};

pub fn init(is_dev: bool) -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = Subscriber::builder()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(if is_dev {
                    LevelFilter::TRACE.into()
                } else {
                    LevelFilter::INFO.into()
                })
                .from_env_lossy(),
        )
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}
