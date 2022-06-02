use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Database {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub charset: String,
}
