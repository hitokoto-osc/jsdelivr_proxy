//! The cache-purge command, shared by the admin API and the webhook.
//!
//! The two entry points authenticate differently and audit differently, but
//! the operation itself is one thing; keeping it here stops the webhook from
//! having to reach into the admin controller for it.

use serde::Serialize;

use super::{clear, invalidate_key, invalidate_prefix};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    /// Every key starting with this string. Cache keys are request paths, so
    /// `gh/owner/repo@HEAD` covers one pinned version of one repository and
    /// `gh/owner/repo` covers all of its versions.
    Prefix(String),
    /// Exactly these keys, i.e. full request paths.
    Keys(Vec<String>),
}

impl Scope {
    /// Short form recorded as the audit entry's target.
    pub fn label(&self) -> String {
        match self {
            Scope::All => "*".to_string(),
            Scope::Prefix(prefix) => format!("{prefix}*"),
            Scope::Keys(keys) => keys.join(", "),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Outcome {
    pub removed: usize,
    /// Keys that were named but not cached. Always zero except for
    /// [`Scope::Keys`], where a caller listing stale paths is worth surfacing
    /// rather than silently reporting success.
    pub missed: usize,
}

pub async fn run(scope: &Scope) -> Outcome {
    match scope {
        Scope::All => Outcome {
            removed: clear().await as usize,
            missed: 0,
        },
        Scope::Prefix(prefix) => Outcome {
            removed: invalidate_prefix(prefix).await,
            missed: 0,
        },
        Scope::Keys(keys) => {
            let mut removed = 0;
            for key in keys {
                if invalidate_key(key).await {
                    removed += 1;
                }
            }
            Outcome {
                removed,
                missed: keys.len() - removed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_describe_the_scope() {
        assert_eq!(Scope::All.label(), "*");
        assert_eq!(
            Scope::Prefix("gh/owner/repo@HEAD".into()).label(),
            "gh/owner/repo@HEAD*"
        );
        assert_eq!(
            Scope::Keys(vec!["npm/vue@3/dist/vue.js".into(), "npm/lodash".into()]).label(),
            "npm/vue@3/dist/vue.js, npm/lodash"
        );
    }
}
