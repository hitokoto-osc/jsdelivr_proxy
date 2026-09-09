//! Picks the frontend assets worth warming out of a listing.
//!
//! Order: hidden paths, `include`, `exclude`, extension. `exclude` runs after
//! `include` so that "take this directory, minus one subtree" works.

use crate::conf::preload::Target;

const WILDCARD: &str = "*";

/// Assets a browser loads directly. JSON is included because this project's
/// primary use case, the hitokoto sentence bundles, is entirely JSON.
///
/// Deliberately excluded: `map` (debug-only and often megabytes), `d.ts`, and
/// documentation. Override per target with `extensions`.
static DEFAULT_EXTENSIONS: phf::Set<&'static str> = phf::phf_set! {
    // scripts
    "js", "mjs", "cjs",
    // styles
    "css",
    // markup and structured data
    "json", "html", "htm", "wasm",
    // images
    "apng", "avif", "bmp", "gif", "ico", "jpeg", "jpg", "png", "svg", "webp",
    // fonts
    "eot", "otf", "ttf", "woff", "woff2",
};

/// `path` is a listing path, always leading with `/` (e.g. `/dist/vue.js`).
pub fn should_preload(target: &Target, path: &str) -> bool {
    let Some(relative) = path.strip_prefix('/') else {
        return false;
    };
    if relative.is_empty() {
        return false;
    }

    // Matched per segment, so dotted filenames like `vue.global.min.js` stay.
    if !target.include_hidden && relative.split('/').any(|seg| seg.starts_with('.')) {
        return false;
    }

    if !matches_any_prefix(&target.include, path, true) {
        return false;
    }
    if matches_any_prefix(&target.exclude, path, false) {
        return false;
    }

    extension_allowed(target, path)
}

/// `when_empty` is the result for an empty list.
///
/// Entries are compared per path segment rather than with a bare
/// `starts_with`, so `/dist` matches `/dist/vue.js` but not `/dist-old/x.js`.
fn matches_any_prefix(list: &[String], path: &str, when_empty: bool) -> bool {
    let mut entries = list
        .iter()
        .map(|entry| normalize_prefix(entry))
        .filter(|entry| !entry.is_empty())
        .peekable();
    if entries.peek().is_none() {
        return when_empty;
    }
    entries.any(|entry| path == entry || path.starts_with(&format!("{}/", entry)))
}

/// Accepts `dist`, `/dist` and `/dist/`, all yielding `/dist`.
fn normalize_prefix(entry: &str) -> String {
    let trimmed = entry.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    }
}

fn extension_allowed(target: &Target, path: &str) -> bool {
    let configured: Vec<&str> = target
        .extensions
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .collect();

    if configured.contains(&WILDCARD) {
        return true;
    }

    let Some(ext) = extension_of(path) else {
        return false;
    };

    if configured.is_empty() {
        DEFAULT_EXTENSIONS.contains(ext.to_ascii_lowercase().as_str())
    } else {
        configured
            .iter()
            .any(|entry| entry.trim_start_matches('.').eq_ignore_ascii_case(ext))
    }
}

/// The dot must sit inside the filename: `.gitignore` is a hidden file, not a
/// file of type `gitignore`.
fn extension_of(path: &str) -> Option<&str> {
    let file = path.rsplit('/').next()?;
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        None
    } else {
        Some(ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn target() -> Target {
        Target {
            provider: "gh".into(),
            name: "hitokoto-osc/sentences-bundle".into(),
            ..Default::default()
        }
    }

    #[test]
    fn default_set_covers_script_style_image_and_font() {
        let target = target();
        for path in [
            "/dist/vue.js",
            "/dist/vue.mjs",
            "/dist/vue.cjs",
            "/dist/style.css",
            "/sentences/a.json",
            "/index.html",
            "/pkg/app.wasm",
            "/img/logo.png",
            "/img/logo.SVG",
            "/img/photo.jpeg",
            "/img/anim.webp",
            "/img/icon.ico",
            "/fonts/iconfont.woff2",
            "/fonts/iconfont.ttf",
            "/fonts/iconfont.eot",
        ] {
            assert!(should_preload(&target, path), "expected {} kept", path);
        }
    }

    #[test]
    fn default_set_drops_non_frontend_files() {
        let target = target();
        for path in [
            "/README.md",
            "/LICENSE",
            "/Cargo.toml",
            "/dist/vue.js.map",
            "/types/index.d.ts",
            "/CNAME",
            "/Dockerfile",
        ] {
            assert!(!should_preload(&target, path), "expected {} dropped", path);
        }
    }

    #[test]
    fn hidden_paths_are_ignored_by_default() {
        let target = target();
        assert!(!should_preload(&target, "/.github/workflows/ci.yml"));
        assert!(!should_preload(&target, "/.vscode/settings.json"));
        assert!(!should_preload(&target, "/.gitignore"));
        assert!(!should_preload(&target, "/.next/static/chunk.js"));
        assert!(should_preload(&target, "/dist/vue.global.min.js"));
    }

    #[test]
    fn hidden_paths_can_be_opted_in() {
        let target = Target {
            include_hidden: true,
            ..target()
        };
        assert!(should_preload(&target, "/.vscode/settings.json"));
        // the extension rule still applies
        assert!(!should_preload(&target, "/.gitignore"));
    }

    #[test]
    fn explicit_extensions_replace_the_default_set() {
        let target = Target {
            extensions: list(&["json"]),
            ..target()
        };
        assert!(should_preload(&target, "/sentences/a.json"));
        assert!(!should_preload(&target, "/dist/vue.js"));
    }

    #[test]
    fn extensions_accept_a_leading_dot_and_any_case() {
        let target = Target {
            extensions: list(&[".JSON", "Js"]),
            ..target()
        };
        assert!(should_preload(&target, "/sentences/a.json"));
        assert!(should_preload(&target, "/dist/vue.JS"));
        assert!(!should_preload(&target, "/dist/style.css"));
    }

    #[test]
    fn wildcard_disables_extension_filtering() {
        let target = Target {
            extensions: list(&["*"]),
            ..target()
        };
        assert!(should_preload(&target, "/LICENSE"));
        assert!(should_preload(&target, "/dist/vue.js.map"));
        // hidden paths still need include_hidden
        assert!(!should_preload(&target, "/.github/workflows/ci.yml"));
    }

    #[test]
    fn include_limits_the_scope() {
        let target = Target {
            include: list(&["/dist", "sentences/"]),
            ..target()
        };
        assert!(should_preload(&target, "/dist/vue.js"));
        assert!(should_preload(&target, "/sentences/a.json"));
        assert!(!should_preload(&target, "/src/index.js"));
    }

    #[test]
    fn include_is_not_a_bare_string_prefix() {
        let target = Target {
            include: list(&["/dist"]),
            ..target()
        };
        assert!(!should_preload(&target, "/dist-old/vue.js"));
        assert!(!should_preload(&target, "/distribution/vue.js"));
    }

    #[test]
    fn exclude_wins_over_include() {
        let target = Target {
            include: list(&["/dist"]),
            exclude: list(&["/dist/legacy"]),
            ..target()
        };
        assert!(should_preload(&target, "/dist/vue.js"));
        assert!(!should_preload(&target, "/dist/legacy/vue.js"));
        assert!(!should_preload(&target, "/dist/legacy"));
    }

    #[test]
    fn blank_prefix_entries_do_not_activate_the_filter() {
        let target = Target {
            include: list(&["", "   ", "/"]),
            exclude: list(&[""]),
            ..target()
        };
        assert!(should_preload(&target, "/src/index.js"));
    }

    #[test]
    fn malformed_paths_are_rejected() {
        let target = target();
        assert!(!should_preload(&target, ""));
        assert!(!should_preload(&target, "/"));
        assert!(!should_preload(&target, "dist/vue.js"));
    }
}
