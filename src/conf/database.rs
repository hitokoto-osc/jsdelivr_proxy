use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Database {
    #[serde(default = "Database::default_host")]
    pub host: String,
    #[serde(default = "Database::default_port")]
    pub port: u16,
    #[serde(default = "Database::default_user")]
    pub user: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub charset: String,
}

impl Database {
    fn default_host() -> String {
        "127.0.0.1".into()
    }

    fn default_port() -> u16 {
        3306
    }

    fn default_user() -> String {
        "root".into()
    }
}

impl Default for Database {
    fn default() -> Self {
        Database {
            host: Database::default_host(),
            port: Database::default_port(),
            user: Database::default_user(),
            password: "".into(),
            database: "".into(),
            prefix: "".into(),
            charset: "".into(),
        }
    }
}
