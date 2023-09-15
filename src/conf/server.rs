use serde::Deserialize;

#[derive(Deserialize)]
pub struct Server {
    pub host: Option<String>,
    pub port: Option<u16>,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            host: Some("0.0.0.0".to_string()),
            port: Some(28319),
        }
    }
}