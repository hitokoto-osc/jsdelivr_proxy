use serde::Deserialize;
#[derive(Deserialize)]
pub struct Redis {
    #[serde(default = "Redis::default_host")]
    pub host: String,
    #[serde(default = "Redis::default_port")]
    pub port: u16,
    pub password: Option<String>,
    pub username: Option<String>,
    #[serde(default = "Redis::default_database")]
    pub database: i64,
}

impl Redis {
    fn default_host() -> String {
        "127.0.0.1".into()
    }

    fn default_port() -> u16 {
        6379
    }

    fn default_database() -> i64 {
        0
    }

    pub fn to_uri(&self) -> String {
        format!(
            "redis://{}{}{}:{}/{}",
            match self.username.clone() {
                Some(v) => v + ":",
                None => "".into(),
            },
            match self.password.clone() {
                Some(v) => v + "@",
                None => "".into(),
            },
            self.host,
            self.port,
            self.database
        )
    }
}

impl Default for Redis {
    fn default() -> Self {
        Redis {
            host: Redis::default_host(),
            port: Redis::default_port(),
            password: None,
            username: None,
            database: Redis::default_database(),
        }
    }
}
