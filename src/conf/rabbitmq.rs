use serde::Deserialize;

#[derive(Deserialize)]
pub struct RabbitMQ {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    #[serde(default)]
    pub vhost: String,
}
