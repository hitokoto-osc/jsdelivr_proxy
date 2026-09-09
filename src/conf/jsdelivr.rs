use serde::Deserialize;

use super::{allowlist::Allowlist, referer::RefererCheck};

#[derive(Deserialize, Debug)]
pub struct Jsdelivr {
    pub mirror: Option<String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    #[serde(default)]
    pub referer_check: RefererCheck,
    /// 资源白名单（反滥用）。默认全空 = 不限制，语义见 [`Allowlist`]。
    #[serde(default)]
    pub allowlist: Allowlist,
}

impl Default for Jsdelivr {
    fn default() -> Self {
        Jsdelivr {
            mirror: Some("https://cdn.jsdelivr.net".into()),
            user_agent: None,
            referer: None,
            referer_check: RefererCheck::default(),
            allowlist: Allowlist::default(),
        }
    }
}
