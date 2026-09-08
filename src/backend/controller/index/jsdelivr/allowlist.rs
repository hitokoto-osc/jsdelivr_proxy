//! 把请求路径解析成 jsDelivr 的「provider + 资源」二元组，并交给
//! [`crate::conf::allowlist::Allowlist`] 判定是否放行。
//!
//! 配置侧只关心"哪些名字被允许"，路径长什么样是 jsDelivr 的领域知识，
//! 因此解析放在 controller 这一侧。

use thiserror::Error;

use crate::conf::allowlist::Allowlist;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum AllowlistDenied {
    #[error("Forbidden: provider `{0}` is not allowed by this proxy")]
    Provider(String),
    #[error("Forbidden: `{0}` is not on this proxy's allowlist")]
    Resource(String),
    #[error("Forbidden: the requested path is not a recognizable jsDelivr resource")]
    Malformed,
}

/// 白名单入口。路径已经过 `validate_path` 的穿越校验，不含前导 `/` 与空片段。
pub fn check(allowlist: &Allowlist, path: &str) -> Result<(), AllowlistDenied> {
    // 未配置任何限制：保持引入本特性之前的行为，直接放行。
    if allowlist.is_unrestricted() {
        return Ok(());
    }
    check_component(allowlist, path, true)
}

/// 校验单个资源引用。
///
/// `allow_combine` 只在最外层为真：jsDelivr 不支持嵌套 combine，
/// 允许嵌套只会给绕过留下额外的解析面。
fn check_component(
    allowlist: &Allowlist,
    raw: &str,
    allow_combine: bool,
) -> Result<(), AllowlistDenied> {
    // combine 的组成部分可能写成 `/npm/xxx` 这种 URL 形态，需要先归一化。
    let component = raw.trim().trim_start_matches('/');
    let (provider, rest) = component
        .split_once('/')
        .ok_or(AllowlistDenied::Malformed)?;

    // `combine/a,b,c` 会在一次请求里打包多个资源。只看最外层的 `combine`
    // 等于对整个白名单开了后门，因此必须拆开逐个校验，任意一个不通过就整体拒绝。
    if provider.eq_ignore_ascii_case("combine") {
        if !allow_combine {
            return Err(AllowlistDenied::Malformed);
        }
        for part in rest.split(',') {
            check_component(allowlist, part, false)?;
        }
        return Ok(());
    }

    if !allowlist.allows_provider(provider) {
        return Err(AllowlistDenied::Provider(provider.to_string()));
    }

    if provider.eq_ignore_ascii_case("npm") {
        let name = npm_package_name(rest).ok_or(AllowlistDenied::Malformed)?;
        if allowlist.allows_npm(&name) {
            Ok(())
        } else {
            Err(AllowlistDenied::Resource(format!("npm/{}", name)))
        }
    } else if provider.eq_ignore_ascii_case("gh") {
        let (owner, repo) = gh_owner_repo(rest).ok_or(AllowlistDenied::Malformed)?;
        if allowlist.allows_gh(owner, repo) {
            Ok(())
        } else {
            Err(AllowlistDenied::Resource(format!("gh/{}/{}", owner, repo)))
        }
    } else {
        // 本代理无法按资源粒度识别的 provider（wp / esm / ...）：
        // 白名单已生效的前提下，只有被显式写进 `providers` 才放行。
        if allowlist.provider_listed(provider) {
            Ok(())
        } else {
            Err(AllowlistDenied::Provider(provider.to_string()))
        }
    }
}

/// 剥离版本号。
///
/// 分隔包名与版本的 `@` 一定不在片段的 0 号位置——0 号位置的 `@` 是 scope 前缀，
/// 所以 `@scope/pkg@1.2.3` 能被正确拆开。
fn strip_version(segment: &str) -> &str {
    match segment
        .char_indices()
        .find(|(index, ch)| *index > 0 && *ch == '@')
    {
        Some((index, _)) => &segment[..index],
        None => segment,
    }
}

/// `vue@3.5.0/dist/vue.js` => `vue`；`@scope/pkg@1.0.0/x.js` => `@scope/pkg`。
fn npm_package_name(rest: &str) -> Option<String> {
    let mut segments = rest.split('/');
    let first = segments.next()?;
    match first.strip_prefix('@') {
        // scope 自成一个片段，包名在下一个片段里
        Some(scope) => {
            let package = strip_version(segments.next()?);
            if scope.is_empty() || package.is_empty() {
                None
            } else {
                Some(format!("@{}/{}", scope, package))
            }
        }
        None => {
            let package = strip_version(first);
            if package.is_empty() {
                None
            } else {
                Some(package.to_string())
            }
        }
    }
}

/// `owner/repo@v1/dist/x.js` => `(owner, repo)`。
fn gh_owner_repo(rest: &str) -> Option<(&str, &str)> {
    let mut segments = rest.split('/');
    let owner = segments.next()?;
    let repo = strip_version(segments.next()?);
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some((owner, repo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    /// 典型的生产配置：只放行自家的 npm 包与 GitHub 仓库。
    fn pinned() -> Allowlist {
        Allowlist {
            providers: list(&["npm", "gh"]),
            npm: list(&["vue", "@hitokoto/core", "@hitokoto"]),
            gh: list(&["hitokoto-osc", "vuejs/core"]),
        }
    }

    fn assert_allowed(allowlist: &Allowlist, path: &str) {
        assert_eq!(check(allowlist, path), Ok(()), "expected {} allowed", path);
    }

    fn assert_denied(allowlist: &Allowlist, path: &str) {
        assert!(check(allowlist, path).is_err(), "expected {} denied", path);
    }

    #[test]
    fn empty_config_allows_everything() {
        let allowlist = Allowlist::default();
        for path in [
            "npm/vue@3.5.0/dist/vue.js",
            "gh/whoever/whatever@main/x.js",
            "wp/some-plugin/tags/1.0/x.js",
            "esm/vue",
            "combine/npm/vue@3,npm/whatever@1",
            "not-a-provider/at-all",
        ] {
            assert_allowed(&allowlist, path);
        }
    }

    #[test]
    fn provider_filtering() {
        let allowlist = Allowlist {
            providers: list(&["npm"]),
            ..Default::default()
        };
        assert_allowed(&allowlist, "npm/anything@1/x.js");
        assert_denied(&allowlist, "gh/whoever/whatever/x.js");
        assert_denied(&allowlist, "wp/some-plugin/x.js");
    }

    #[test]
    fn provider_matching_is_case_insensitive() {
        let allowlist = pinned();
        assert_allowed(&allowlist, "NPM/vue@3.5.0/dist/vue.js");
        assert_allowed(&allowlist, "GH/HITOKOTO-OSC/sentences-bundle@v1/x.json");
    }

    #[test]
    fn npm_exact_package() {
        let allowlist = pinned();
        assert_allowed(&allowlist, "npm/vue");
        assert_allowed(&allowlist, "npm/vue@3.5.0/dist/vue.js");
        assert_denied(&allowlist, "npm/lodash@4/lodash.min.js");
    }

    #[test]
    fn npm_scope_wildcard() {
        let allowlist = pinned();
        assert_allowed(&allowlist, "npm/@hitokoto/core@1.0.0/index.js");
        assert_allowed(&allowlist, "npm/@hitokoto/sentences-bundle@1/all.json");
    }

    /// 前缀匹配是白名单最经典的绕过方式，必须按片段整体比较。
    #[test]
    fn npm_scope_is_not_a_prefix_match() {
        let allowlist = pinned();
        assert_denied(&allowlist, "npm/@hitokoto-evil/core@1.0.0/index.js");
        assert_denied(&allowlist, "npm/vue-evil@1/x.js");
    }

    #[test]
    fn gh_owner_wildcard_and_exact_repo() {
        let allowlist = pinned();
        assert_allowed(
            &allowlist,
            "gh/hitokoto-osc/sentences-bundle@v1.0.0/all.json",
        );
        assert_allowed(&allowlist, "gh/hitokoto-osc/anything/x.js");
        assert_allowed(&allowlist, "gh/vuejs/core@3/dist/vue.js");
        assert_denied(&allowlist, "gh/vuejs/vue@2/dist/vue.js");
    }

    #[test]
    fn gh_owner_is_not_a_prefix_match() {
        let allowlist = pinned();
        assert_denied(&allowlist, "gh/hitokoto-osc-evil/sentences-bundle/all.json");
        assert_denied(&allowlist, "gh/hitokoto-osc-evil/x@1/y.js");
    }

    /// 版本号必须先剥离，否则 `vue@3.5.0` 永远匹配不上 `vue`。
    #[test]
    fn version_specifier_is_stripped() {
        let allowlist = pinned();
        assert_allowed(&allowlist, "npm/vue@3.5.0/dist/vue.js");
        assert_allowed(&allowlist, "npm/@hitokoto/core@latest/index.js");
        assert_allowed(&allowlist, "gh/vuejs/core@3.4.0/dist/vue.js");
    }

    /// combine 会在一次请求里打包多个资源；只要有一个不在白名单内就整体拒绝，
    /// 否则它就是绕过整个白名单的捷径。
    #[test]
    fn combine_denies_when_any_component_is_not_allowed() {
        let allowlist = pinned();
        assert_allowed(
            &allowlist,
            "combine/npm/vue@3.5.0/dist/vue.js,npm/@hitokoto/core@1/index.js",
        );
        assert_denied(
            &allowlist,
            "combine/npm/vue@3.5.0/dist/vue.js,npm/lodash@4/lodash.min.js",
        );
        assert_denied(
            &allowlist,
            "combine/npm/vue@3.5.0/dist/vue.js,gh/hitokoto-osc-evil/x@1/y.js",
        );
        // URL 形态的分量（分量自带前导斜杠）同样要被拆开校验
        assert_denied(&allowlist, "combine/npm/vue@3,/npm/lodash@4/lodash.js");
        // 嵌套 combine 不是合法的 jsDelivr 用法，直接拒绝
        assert_denied(&allowlist, "combine/combine/npm/vue@3,npm/vue@3");
    }

    #[test]
    fn unknown_or_malformed_provider_is_denied_under_restriction() {
        let allowlist = pinned();
        assert_denied(&allowlist, "wp/some-plugin/tags/1.0/x.js");
        assert_denied(&allowlist, "esm/vue");
        assert_denied(&allowlist, "npm"); // 缺少包名
        assert_denied(&allowlist, "npm/@hitokoto"); // scoped 包缺少包名
        assert_denied(&allowlist, "gh/hitokoto-osc"); // 缺少仓库名
        assert_denied(&allowlist, "combine/npm/vue@3,"); // 空分量
    }

    /// 未在 `npm`/`gh` 中列出资源、只限制 provider 时，该 provider 下的资源全部放行；
    /// 无法按资源粒度识别的 provider 需要显式写进 `providers` 才能放行。
    #[test]
    fn unrecognized_provider_can_be_opted_in_explicitly() {
        let allowlist = Allowlist {
            providers: list(&["npm", "wp"]),
            npm: list(&["vue"]),
            ..Default::default()
        };
        assert_allowed(&allowlist, "wp/some-plugin/tags/1.0/x.js");
        assert_denied(&allowlist, "esm/vue");
        assert_denied(&allowlist, "npm/lodash");
    }

    #[test]
    fn denial_reason_is_reported() {
        let allowlist = pinned();
        assert_eq!(
            check(&allowlist, "npm/lodash@4/lodash.js"),
            Err(AllowlistDenied::Resource("npm/lodash".into()))
        );
        assert_eq!(
            check(&allowlist, "wp/plugin/x.js"),
            Err(AllowlistDenied::Provider("wp".into()))
        );
        assert_eq!(check(&allowlist, "npm"), Err(AllowlistDenied::Malformed));
    }
}
