use serde::Deserialize;

/// jsDelivr 资源白名单（反滥用开关）。
///
/// 默认（三个列表全空）**不做任何限制**，与引入本特性之前的行为完全一致；
/// 只要任意一个列表非空，白名单即生效。
///
/// * `providers`：允许的 provider（`npm` / `gh` / `wp` / ...），空 = 不限制 provider；
/// * `npm`：允许的 npm 包名。`vue` 精确匹配包名；`@scope` 匹配该 scope 下的全部包；
/// * `gh`：允许的 GitHub 仓库。`owner` 匹配该 owner 的全部仓库；`owner/repo` 精确匹配。
///
/// 匹配一律**不区分大小写**（GitHub 的 owner/repo 大小写不敏感，npm 包名本身是小写），
/// 且**严格按片段整体比较**——`hitokoto-osc` 不会匹配到 `hitokoto-osc-evil`，
/// `@hitokoto` 也不会匹配到 `@hitokoto-evil`。
#[derive(Deserialize, Debug, Default)]
pub struct Allowlist {
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub npm: Vec<String>,
    #[serde(default)]
    pub gh: Vec<String>,
}

/// 列表是否为空。写配置时容易留下空串（TOML 里的 `""`、环境变量里多余的逗号），
/// 这些条目不应该被当成"限制已生效"的信号。
fn is_blank(list: &[String]) -> bool {
    list.iter().all(|entry| entry.trim().is_empty())
}

fn entries(list: &[String]) -> impl Iterator<Item = &str> {
    list.iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
}

impl Allowlist {
    /// 未配置任何限制：放行一切请求（默认行为）。
    pub fn is_unrestricted(&self) -> bool {
        is_blank(&self.providers) && is_blank(&self.npm) && is_blank(&self.gh)
    }

    /// provider 是否被显式写进 `providers`。
    ///
    /// 对于本代理无法按资源粒度识别的 provider（`wp`、`esm` 等），
    /// 白名单生效时只有显式列出才放行，避免出现"限制了 npm 却能用 wp 打穿"的口子。
    pub fn provider_listed(&self, provider: &str) -> bool {
        entries(&self.providers).any(|entry| entry.eq_ignore_ascii_case(provider))
    }

    /// `providers` 这一层是否放行该 provider（列表为空 = 全部放行）。
    pub fn allows_provider(&self, provider: &str) -> bool {
        is_blank(&self.providers) || self.provider_listed(provider)
    }

    /// `name` 是已剥离版本号的完整包名，可能形如 `@scope/pkg`。
    pub fn allows_npm(&self, name: &str) -> bool {
        if is_blank(&self.npm) {
            return true;
        }
        let scope = name
            .strip_prefix('@')
            .and_then(|rest| rest.split_once('/'))
            .map(|(scope, _)| scope);
        entries(&self.npm).any(|entry| {
            match entry
                .strip_prefix('@')
                .filter(|scope_entry| !scope_entry.contains('/'))
            {
                // `@scope` 形式：匹配该 scope 下的所有包
                Some(entry_scope) => {
                    scope.is_some_and(|scope| scope.eq_ignore_ascii_case(entry_scope))
                }
                // 其余形式：整包名精确匹配
                None => entry.eq_ignore_ascii_case(name),
            }
        })
    }

    /// `repo` 是已剥离版本号 / 分支名的仓库名。
    pub fn allows_gh(&self, owner: &str, repo: &str) -> bool {
        if is_blank(&self.gh) {
            return true;
        }
        entries(&self.gh).any(|entry| match entry.split_once('/') {
            Some((entry_owner, entry_repo)) => {
                entry_owner.eq_ignore_ascii_case(owner) && entry_repo.eq_ignore_ascii_case(repo)
            }
            None => entry.eq_ignore_ascii_case(owner),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn default_is_unrestricted() {
        let allowlist = Allowlist::default();
        assert!(allowlist.is_unrestricted());
        assert!(allowlist.allows_provider("npm"));
        assert!(allowlist.allows_npm("anything"));
        assert!(allowlist.allows_gh("anyone", "anything"));
    }

    #[test]
    fn blank_entries_do_not_activate_the_allowlist() {
        let allowlist = Allowlist {
            providers: list(&["", "   "]),
            npm: list(&[""]),
            gh: Vec::new(),
        };
        assert!(allowlist.is_unrestricted());
    }

    #[test]
    fn provider_list_filters() {
        let allowlist = Allowlist {
            providers: list(&["npm", "gh"]),
            ..Default::default()
        };
        assert!(allowlist.allows_provider("npm"));
        assert!(allowlist.allows_provider("GH")); // 不区分大小写
        assert!(!allowlist.allows_provider("wp"));
    }

    #[test]
    fn empty_package_list_allows_every_package_of_that_provider() {
        let allowlist = Allowlist {
            providers: list(&["npm", "gh"]),
            ..Default::default()
        };
        assert!(allowlist.allows_npm("whatever"));
        assert!(allowlist.allows_gh("whoever", "whatever"));
    }

    #[test]
    fn npm_exact_and_scope_matching() {
        let allowlist = Allowlist {
            npm: list(&["vue", "@hitokoto/core", "@scoped"]),
            ..Default::default()
        };
        assert!(allowlist.allows_npm("vue"));
        assert!(allowlist.allows_npm("VUE")); // 不区分大小写
        assert!(allowlist.allows_npm("@hitokoto/core"));
        assert!(allowlist.allows_npm("@scoped/anything"));
        assert!(allowlist.allows_npm("@SCOPED/other"));

        assert!(!allowlist.allows_npm("vue-router")); // 不允许前缀匹配
        assert!(!allowlist.allows_npm("@hitokoto/sentences-bundle"));
        assert!(!allowlist.allows_npm("@scoped-evil/pkg")); // scope 必须整体相等
        assert!(!allowlist.allows_npm("scoped")); // 无 scope 的同名包不算命中
    }

    #[test]
    fn gh_owner_wildcard_and_exact_repo() {
        let allowlist = Allowlist {
            gh: list(&["hitokoto-osc", "vuejs/core"]),
            ..Default::default()
        };
        assert!(allowlist.allows_gh("hitokoto-osc", "sentences-bundle"));
        assert!(allowlist.allows_gh("Hitokoto-OSC", "anything")); // 不区分大小写
        assert!(allowlist.allows_gh("vuejs", "core"));

        assert!(!allowlist.allows_gh("hitokoto-osc-evil", "x")); // owner 必须整体相等
        assert!(!allowlist.allows_gh("vuejs", "core-evil")); // repo 必须整体相等
        assert!(!allowlist.allows_gh("vuejs", "vue"));
    }
}
