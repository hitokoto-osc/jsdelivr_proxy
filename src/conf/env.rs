use serde::Deserialize;
use std::fmt;

#[derive(Deserialize)]
pub enum Environment {
    Development,
    Production,
    Testing,
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Environment::Development => write!(f, "Development"),
            Environment::Testing => write!(f, "Testing"),
            Environment::Production => write!(f, "Production"),
        }
    }
}

impl From<&str> for Environment {
    fn from(env: &str) -> Self {
        match env {
            "Testing" => Environment::Testing,
            "Production" => Environment::Production,
            _ => Environment::Development,
        }
    }
}
