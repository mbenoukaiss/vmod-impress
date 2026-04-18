use std::path::{Component, Path, PathBuf};

/// Resolve `rel` under `root_canon` and reject any path that escapes the root.
///
/// Returns None when:
/// - `rel` contains a parent (`..`) or absolute prefix component
/// - the joined path doesn't exist (canonicalize fails)
/// - canonicalization resolves to a path outside `root_canon` (e.g. symlink escape)
///
/// `root_canon` MUST already be canonical (caller's responsibility — typically
/// done once at config-load time so the per-request hot path stays cheap).
pub fn safe_join(root_canon: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.components().any(|c| matches!(
        c,
        Component::ParentDir | Component::RootDir | Component::Prefix(_)
    )) {
        return None;
    }
    let joined = root_canon.join(rel_path);
    let canon = std::fs::canonicalize(&joined).ok()?;
    if !canon.starts_with(root_canon) {
        return None;
    }
    Some(canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rejects_parent_traversal() {
        let root = TempDir::new().unwrap();
        let root_canon = std::fs::canonicalize(root.path()).unwrap();
        assert!(safe_join(&root_canon, "../etc/passwd").is_none());
        assert!(safe_join(&root_canon, "a/../../etc/passwd").is_none());
    }

    #[test]
    fn rejects_absolute_path() {
        let root = TempDir::new().unwrap();
        let root_canon = std::fs::canonicalize(root.path()).unwrap();
        assert!(safe_join(&root_canon, "/etc/passwd").is_none());
    }

    #[test]
    fn resolves_simple_path() {
        let root = TempDir::new().unwrap();
        let root_canon = std::fs::canonicalize(root.path()).unwrap();
        let f = root_canon.join("a/b/c.txt");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "x").unwrap();
        assert_eq!(
            safe_join(&root_canon, "a/b/c.txt").unwrap(),
            std::fs::canonicalize(&f).unwrap(),
        );
    }

    #[test]
    fn missing_file_returns_none() {
        let root = TempDir::new().unwrap();
        let root_canon = std::fs::canonicalize(root.path()).unwrap();
        assert!(safe_join(&root_canon, "nope.txt").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn rejects_symlink_escape() {
        let root = TempDir::new().unwrap();
        let root_canon = std::fs::canonicalize(root.path()).unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("secret");
        std::fs::write(&outside_file, "x").unwrap();
        std::os::unix::fs::symlink(&outside_file, root_canon.join("link")).unwrap();
        assert!(safe_join(&root_canon, "link").is_none());
    }
}
