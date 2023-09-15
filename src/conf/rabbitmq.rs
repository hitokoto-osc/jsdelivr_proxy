use serde::Deserialize;

#[derive(Deserialize)]
pub struct RabbitMQ {
    #[serde(default = "RabbitMQ::default_host")]
    pub host: String,
    #[serde(default = "RabbitMQ::default_port")]
    pub port: u16,
    #[serde(default = "RabbitMQ::default_user")]
    pub user: String,
    #[serde(default = "RabbitMQ::default_pass")]
    pub password: String,
    #[serde(default)]
    pub vhost: String,
}

impl RabbitMQ {
    pub fn default_host() -> String {
        "127.0.0.1".into()
    }

    pub fn default_port() -> u16 {
        5672
    }

    pub fn default_user() -> String {
        "myuser".into()
    }

    pub fn default_pass() -> String {
        "mypass".into()
    }
}

impl Default for RabbitMQ {
    fn default() -> Self {
        RabbitMQ {
            host: RabbitMQ::default_host(),
            port: RabbitMQ::default_port(),
            user: RabbitMQ::default_user(),
            password: RabbitMQ::default_pass(),
            vhost: "".into(),
        }
    }
}
