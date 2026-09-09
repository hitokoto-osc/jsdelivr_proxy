use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct Gravatar {
    pub upstream: String,
}

impl Default for Gravatar {
    fn default() -> Self {
        Self {
            upstream: "https://www.gravatar.com".into(),
        }
    }
}
