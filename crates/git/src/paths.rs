//! A checkout's paths: their components, their depth, and a short label
//! for a log line.

/// The path's components, `.` dropped and `..` resolved against the
/// component before it.
///
/// None when the path is absolute, or a `..` would climb above the
/// checkout's root.
#[must_use]
pub fn normalised(path: &str) -> Option<Vec<String>> {
    if path.starts_with('/') {
        return None;
    }

    let mut parts = Vec::new();

    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name.to_owned()),
        }
    }

    Some(parts)
}

/// How many components the path names, `.` and `..` resolved.
///
/// A path that cannot be normalised, such as an absolute one, names no
/// components.
#[must_use]
pub fn depth(path: &str) -> usize {
    normalised(path).unwrap_or_default().len()
}

/// The path without its last component, the checkout's root being the
/// empty path.
///
/// None when the path names no component, or cannot be normalised.
#[must_use]
pub fn parent(path: &str) -> Option<String> {
    let mut parts = normalised(path)?;
    parts.pop()?;

    Some(parts.join("/"))
}

/// `path` relative to `root`, both taken from the checkout's top.
///
/// None when either path cannot be normalised, or `path` is neither `root`
/// nor below it.
#[must_use]
pub fn under(root: &str, path: &str) -> Option<String> {
    let root = normalised(root)?;
    let parts = normalised(path)?;
    let rest = parts.strip_prefix(root.as_slice())?;

    Some(rest.join("/"))
}

/// A short label for the path, as a log line shows it: the folder at the
/// top, then the last two components.
///
/// The checkout's root is `.`, and a path that cannot be normalised is
/// returned as it is.
#[must_use]
pub fn label(path: &str) -> String {
    let Some(parts) = normalised(path) else {
        return path.to_owned();
    };

    match parts.as_slice() {
        [] => ".".to_owned(),
        [only] => (*only).clone(),
        [folder, file] => format!("{folder}/{file}"),
        [first, .., folder, file] => format!("{first}/{folder}/{file}"),
    }
}

/// Whether both paths start at the same component: the same folder at the
/// checkout's top, or both at its root.
///
/// A path that cannot be normalised shares nothing.
#[must_use]
pub fn shares_root(path: &str, other: &str) -> bool {
    let Some(path) = normalised(path) else {
        return false;
    };
    let Some(other) = normalised(other) else {
        return false;
    };

    path.first() == other.first()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_and_dot_dot_are_resolved_and_dropped() {
        assert_eq!(
            normalised("crates/./git//src/"),
            Some(vec!["crates".into(), "git".into(), "src".into()])
        );
        assert_eq!(
            normalised("crates/git/../core"),
            Some(vec!["crates".into(), "core".into()])
        );
        assert_eq!(normalised("."), Some(vec![]));
    }

    #[test]
    fn a_path_above_the_root_or_from_it_is_refused() {
        assert_eq!(normalised(".."), None);
        assert_eq!(normalised("crates/../../core"), None);
        assert_eq!(normalised("/crates/git"), None);
        assert_eq!(normalised("/"), None);
    }

    #[test]
    fn depth_counts_the_components_a_path_names() {
        assert_eq!(depth("."), 0);
        assert_eq!(depth("Cargo.toml"), 1);
        assert_eq!(depth("crates/git/src/paths.rs"), 4);
        assert_eq!(depth("crates/git/../core"), 2);
        assert_eq!(depth("/crates/git"), 0);
    }

    #[test]
    fn a_parent_is_the_path_without_its_last_component() {
        assert_eq!(
            parent("crates/git/src/paths.rs").as_deref(),
            Some("crates/git/src")
        );
        assert_eq!(parent("Cargo.toml").as_deref(), Some(""));
        assert_eq!(parent("crates/../.."), None);
        assert_eq!(parent("."), None);
    }

    #[test]
    fn a_path_under_a_root_is_what_follows_it() {
        assert_eq!(
            under("crates", "crates/git/src").as_deref(),
            Some("git/src")
        );
        assert_eq!(under("crates/git", "crates/git").as_deref(), Some(""));
        assert_eq!(under(".", "crates/git").as_deref(), Some("crates/git"));
    }

    #[test]
    fn a_path_outside_its_root_is_none() {
        assert_eq!(under("crates/git", "crates"), None);
        assert_eq!(under("crates", "crates-other/git"), None);
        assert_eq!(under("crates", "crates/git/../../git"), None);
        assert_eq!(under("crates", "/crates/git"), None);
    }

    #[test]
    fn a_label_names_the_top_and_the_last_two_components() {
        assert_eq!(label("."), ".");
        assert_eq!(label("Cargo.toml"), "Cargo.toml");
        assert_eq!(label("crates/git"), "crates/git");
        assert_eq!(label("crates/git/src"), "crates/git/src");
        assert_eq!(label("crates/git/src/paths.rs"), "crates/src/paths.rs");
        assert_eq!(label("/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn paths_share_a_root_when_their_first_component_matches() {
        assert!(shares_root("crates/git/src", "crates/core/src"));
        assert!(shares_root(".", "."));
        assert!(!shares_root("crates", "crates-core"));
        assert!(!shares_root("crates", "."));
        assert!(!shares_root("..", ".."));
        assert!(!shares_root("/crates", "crates"));
    }
}
