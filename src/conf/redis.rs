use serde::Deserialize;
#[derive(Deserialize)]
pub struct Redis {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
    pub username: Option<String>,
    pub database: i64,
}

impl Redis {
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
