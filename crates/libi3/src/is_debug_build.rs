//! C: libi3/is_debug_build.c

use crate::i3_version;

/// C: libi3/is_debug_build.c:17 — Returns true if this version of i3 is a
/// debug build (anything which is not a release version), based on the git
/// version number.
///
/// i3_version contains either something like this:
///     "4.0.2 (2011-11-11)" (release version)
/// or: "4.0.2-123-gC0FFEE"  (debug version)
///
/// So we check for the offset of the first opening round bracket to
/// determine whether this is a git version or a release version.
pub fn is_debug_build() -> bool {
    let version = i3_version();
    match version.find('(') {
        None => true, // e.g. 4.0.2-123-gC0FFEE
        // In practice, debug versions do not contain parentheses at all,
        // but leave the logic as it was before so that we can re-add
        // parentheses if we chose to.
        Some(pos) => pos > 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::test_util::env_lock()
    }

    #[test]
    fn non_git_version_is_debug_build() {
        let _g = lock_env();
        std::env::remove_var("I3RS_VERSION");
        // "4.25-non-git" has no '(' → debug build
        assert!(is_debug_build());
    }

    #[test]
    fn release_version_is_not_debug_build() {
        let _g = lock_env();
        std::env::set_var("I3RS_VERSION", "4.25.1 (2026-02-06)");
        // '(' at offset 7, not > 10 → release build
        assert!(!is_debug_build());
    }
}
