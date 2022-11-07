mod controller;

use crate::{conf::env::Environment, CONFIG};
use controller::*;
use rocket::{
    figment::{
        providers::{Env, Format, Toml},
        Figment, Profile,
    },
    log::LogLevel,
    routes, Config,
};

fn config_provider() -> rocket::figment::Figment {
    Figment::from(Config::default())
        .merge(("log_level", LogLevel::Normal))
        .merge(Toml::file(Env::var_or("ROCKET_CONFIG", "Rocket.toml")).nested())
        .merge(Env::prefixed("ROCKET_").ignore(&["PROFILE"]).global())
        .merge((
            "port",
            match &CONFIG.server.port {
                Some(v) => *v,
                None => 8000,
            },
        ))
        .merge((
            "host",
            match &CONFIG.server.host {
                Some(v) => v.to_owned(),
                None => "0.0.0.0".to_string(),
            },
        ))
        .select(Profile::from_env_or(
            "ROCKET_PROFILE",
            match &CONFIG.env {
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
