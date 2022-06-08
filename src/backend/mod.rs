mod controller;

use crate::{conf::env::Environment, CONFIG};
use controller::*;
use rocket::{figment::Profile, routes, Config};

fn config_provider() -> rocket::figment::Figment {
    Config::figment().select(Profile::from_env_or(
        "ROCKET_PROFILE",
        match &(*CONFIG).env {
            Environment::Production => Config::RELEASE_PROFILE,
            _ => Config::DEBUG_PROFILE,
        },
    ))
}

pub async fn init() -> Result<(), rocket::Error> {
    let _rocket = rocket::custom(config_provider())
        .mount(
            "/",
            routes![
                index::index,
                index::favicon,
                index::about,
                index::jsdelivr::get
            ],
        )
        .ignite()
        .await?
        .launch()
        .await?;

    Ok(())
}
