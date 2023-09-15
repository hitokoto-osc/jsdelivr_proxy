use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Jsdelivr {
    pub mirror: Option<String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
}

impl Default for Jsdelivr {
    fn default() -> Self {
        Jsdelivr {
            mirror: Some("https://cdn.jsdelivr.net".into()),
            user_agent: None,
            referer: None,
        }
    }
}
