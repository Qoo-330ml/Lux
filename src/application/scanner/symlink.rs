//! Symbolic-link policy for media discovery.
//!
//! Discovery never follows a link by accident: every decision goes through
//! [`SymlinkPolicy`], so the manifest scan, the reconciliation scan and the
//! incremental watch agree with each other.
//!
//! * file links are always resolved (the link name decides candidacy, the
//!   target decides size, mtime and identity);
//! * directory links are only entered according to [`DirectoryLinkPolicy`];
//! * a resolved target must stay inside the library root that owns the link, or
//!   inside one of the explicitly configured target roots.
//!
//! The default is `root_children`: a media root added by the user may itself be
//! a link or contain linked subdirectories one level deep, which is how people
//! stitch several disks into one shelf. Deeper links stay ignored because they
//! duplicate whole subtrees and are impossible to explain in a library UI.

use std::{
    env,
    path::{Path, PathBuf},
    sync::OnceLock,
};

const POLICY_ENV: &str = "LUX_SYMLINK_POLICY";
const TARGET_ROOTS_ENV: &str = "LUX_SYMLINK_TARGET_ROOTS";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryLinkPolicy {
    /// Never enter a linked directory.
    None,
    /// Enter a linked directory only when it is a direct child of a media root.
    RootChildren,
    /// Enter any linked directory, still guarded by the visited set.
    All,
}

impl DirectoryLinkPolicy {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "root_children" | "root-children" | "rootchildren" => Some(Self::RootChildren),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SymlinkPolicy {
    directories: DirectoryLinkPolicy,
    allowed_target_roots: Vec<PathBuf>,
}

impl SymlinkPolicy {
    pub(super) fn new(
        directories: DirectoryLinkPolicy,
        allowed_target_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            directories,
            allowed_target_roots,
        }
    }

    /// Follow file links only and keep every target inside the owning root.
    #[cfg(test)]
    pub(super) fn strict() -> Self {
        Self::new(DirectoryLinkPolicy::None, Vec::new())
    }

    /// Process-wide policy derived from the environment, parsed once.
    pub(crate) fn current() -> &'static Self {
        static POLICY: OnceLock<SymlinkPolicy> = OnceLock::new();
        POLICY.get_or_init(Self::from_env)
    }

    fn from_env() -> Self {
        let directories = match env::var(POLICY_ENV).ok().as_deref() {
            Some(value) => DirectoryLinkPolicy::parse(value).unwrap_or_else(|| {
                tracing::warn!(
                    value,
                    "unrecognised LUX_SYMLINK_POLICY; falling back to root_children"
                );
                DirectoryLinkPolicy::RootChildren
            }),
            None => DirectoryLinkPolicy::RootChildren,
        };
        Self::new(
            directories,
            parse_target_roots(env::var_os(TARGET_ROOTS_ENV)),
        )
    }

    /// `at_root` is true while reading a media root itself, where every entry is
    /// a direct child of that root.
    pub(crate) fn follows_directory_at(&self, at_root: bool) -> bool {
        match self.directories {
            DirectoryLinkPolicy::None => false,
            DirectoryLinkPolicy::RootChildren => at_root,
            DirectoryLinkPolicy::All => true,
        }
    }

    /// A followed target is acceptable only when it resolves inside the library
    /// root that owns the link, or inside an explicitly configured target root.
    ///
    /// Both arguments must already be canonicalized paths.
    pub(crate) fn allows_target(&self, canonical_root: &Path, canonical_target: &Path) -> bool {
        if canonical_target.starts_with(canonical_root) {
            return true;
        }
        self.allowed_target_roots
            .iter()
            .any(|allowed| canonical_target.starts_with(allowed))
    }
}

/// Whether a canonical provider path may be served or probed for `canonical_root`.
///
/// The default range is the library root that owns the link. Targets outside it
/// are served only when the operator listed their root in
/// `LUX_SYMLINK_TARGET_ROOTS`.
pub(crate) fn path_is_allowed(canonical_root: &Path, canonical_path: &Path) -> bool {
    SymlinkPolicy::current().allows_target(canonical_root, canonical_path)
}

fn parse_target_roots(configured: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let Some(configured) = configured else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for path in env::split_paths(&configured) {
        if path.as_os_str().is_empty() {
            continue;
        }
        if !path.is_absolute() {
            tracing::warn!(
                path = %path.display(),
                "symbolic-link target root is not absolute and was ignored"
            );
            continue;
        }
        // Canonicalize so containment compares like with like. A root that does
        // not exist yet is kept verbatim; it simply never matches.
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        roots.push(canonical);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::{DirectoryLinkPolicy, SymlinkPolicy};
    use std::path::{Path, PathBuf};

    #[test]
    fn root_children_enters_only_direct_child_directories() {
        let policy = SymlinkPolicy::new(DirectoryLinkPolicy::RootChildren, Vec::new());
        assert!(policy.follows_directory_at(true));
        assert!(!policy.follows_directory_at(false));
    }

    #[test]
    fn none_and_all_are_unconditional() {
        let none = SymlinkPolicy::new(DirectoryLinkPolicy::None, Vec::new());
        assert!(!none.follows_directory_at(true));
        assert!(!none.follows_directory_at(false));
        let all = SymlinkPolicy::new(DirectoryLinkPolicy::All, Vec::new());
        assert!(all.follows_directory_at(true));
        assert!(all.follows_directory_at(false));
    }

    #[test]
    fn strict_policy_allows_only_the_owning_root() {
        let policy = SymlinkPolicy::strict();
        assert!(policy.allows_target(Path::new("/media/movies"), Path::new("/media/movies/a.mkv")));
        assert!(!policy.allows_target(Path::new("/media/movies"), Path::new("/etc/passwd")));
    }

    #[test]
    fn configured_target_roots_widen_the_allowed_range() {
        let policy = SymlinkPolicy::new(
            DirectoryLinkPolicy::RootChildren,
            vec![PathBuf::from("/mnt/disk2")],
        );
        assert!(policy.allows_target(Path::new("/media/movies"), Path::new("/mnt/disk2/a.mkv")));
        assert!(!policy.allows_target(Path::new("/media/movies"), Path::new("/etc/passwd")));
    }
}
