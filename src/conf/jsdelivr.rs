use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Jsdelivr {
    pub mirror: Option<String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>
}