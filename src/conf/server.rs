use serde::Deserialize;

#[derive(Deserialize)]
pub struct Server {
    pub host: Option<String>,
    pub port: Option<u16>,
}