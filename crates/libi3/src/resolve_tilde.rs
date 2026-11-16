//! C: libi3/resolve_tilde.c

use std::env;
use std::ffi::{CStr, CString};

/// Result of the C glob() call in resolve_tilde.
enum GlobResult {
    /// C: res == 0 && gl_pathc == 1 — a single match.
    Match(String),
    /// C: res == GLOB_NOMATCH || gl_pathc != 1 — no match, or many wildcard
    /// matches (both are bad), or a literal pattern (see below).
    NoMatch,
}

/// C: libi3/resolve_tilde.c:21 — This function resolves ~ in pathnames.
/// It may resolve wildcards in the first part of the path, but if no match
/// or multiple matches are found, it just returns a copy of path as given.
pub fn resolve_tilde(path: &str) -> String {
    // C: tail = strchr(path, '/');
    let tail_pos = path.find('/');
    // C: head = sstrndup(path, tail ? tail - path : strlen(path));
    let head = match tail_pos {
        Some(p) => &path[..p],
        None => path,
    };

    // C: int res = glob(head, GLOB_TILDE, NULL, &globbuf);
    match glob_tilde(head) {
        GlobResult::NoMatch => {
            // no match, or many wildcard matches are bad
            path.to_string()
        }
        GlobResult::Match(path0) => {
            // C: result = scalloc(strlen(head) + strlen(tail) + 1, 1);
            //    strcpy(result, head); if (tail) strcat(result, tail);
            let mut result = path0;
            if let Some(p) = tail_pos {
                result.push_str(&path[p..]);
            }
            result
        }
    }
    // (C: res values other than 0/GLOB_NOMATCH die with
    //  err(EXIT_FAILURE, "glob() failed") — unreachable for this
    //  implementation, which cannot fail that way.)
}

/// The glob(head, GLOB_TILDE, ...) call of C: libi3/resolve_tilde.c:29.
///
/// Verified against glibc's glob(3):
/// - "~" expands to $HOME (falling back to the passwd entry of the current
///   user); the expansion is reported as a match even if the directory does
///   not exist.
/// - "~user" expands via getpwnam; if the user does not exist glibc returns
///   the unchanged pattern as a match, so resolve_tilde copies the path.
/// - glob("") is GLOB_NOMATCH (e.g. for absolute paths, where head is empty).
/// - a pattern without wildcards matches only if the path exists — but
///   whether it matches or not, resolve_tilde's output is the path
///   unchanged, so literal heads take the NoMatch path here.
/// - a pattern with wildcards (* ? [) matches directory entries in the
///   current directory; exactly one match is used, zero or multiple matches
///   lead to NoMatch.
fn glob_tilde(pattern: &str) -> GlobResult {
    if pattern.is_empty() {
        // glob("") → GLOB_NOMATCH (head of an absolute path).
        return GlobResult::NoMatch;
    }

    // GLOB_TILDE expansion of a leading "~" or "~user".
    let effective: String = if pattern == "~" {
        home_dir()
    } else if let Some(user) = pattern.strip_prefix('~') {
        // Unknown user → glibc globs the unchanged pattern.
        getpwnam_home(user).unwrap_or_else(|| pattern.to_string())
    } else {
        pattern.to_string()
    };

    // Wildcard matching (only reachable for patterns without a tilde, since
    // passwd entries and $HOME contain no wildcards).
    if effective.contains('*') || effective.contains('?') || effective.contains('[') {
        let matches = glob_component(&effective);
        if matches.len() == 1 {
            return GlobResult::Match(matches[0].clone());
        }
        return GlobResult::NoMatch;
    }

    if pattern.starts_with('~') {
        // Tilde expansion: a single match (the expansion itself, even when
        // the directory does not exist).
        GlobResult::Match(effective)
    } else {
        // Literal pattern without tilde: whether glob() matches it or
        // reports GLOB_NOMATCH, resolve_tilde's output is the path as given.
        GlobResult::NoMatch
    }
}

/// Matches a single-component wildcard pattern against the entries of the
/// current directory (the C glob() without GLOB_NOSORT sorts the matches).
fn glob_component(pattern: &str) -> Vec<String> {
    let mut matches = Vec::new();
    if let Ok(entries) = std::fs::read_dir(".") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if fnmatch_component(pattern, &name) {
                matches.push(name);
            }
        }
    }
    matches.sort();
    matches
}

/// fnmatch(3) for a single path component: '*' (any sequence), '?'
/// (one character), '[...]' classes with ranges and '!' negation. Like
/// glob(3), a leading '.' must be matched explicitly.
fn fnmatch_component(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    fn match_bytes(p: &[u8], n: &[u8]) -> bool {
        if p.is_empty() {
            return n.is_empty();
        }
        match p[0] {
            b'*' => {
                for i in 0..=n.len() {
                    if match_bytes(&p[1..], &n[i..]) {
                        return true;
                    }
                }
                false
            }
            b'?' => !n.is_empty() && match_bytes(&p[1..], &n[1..]),
            b'[' => {
                if n.is_empty() {
                    return false;
                }
                match match_class(p, n[0]) {
                    Some((matched, rest)) => matched && match_bytes(rest, &n[1..]),
                    None => false,
                }
            }
            c => !n.is_empty() && n[0] == c && match_bytes(&p[1..], &n[1..]),
        }
    }
    match_bytes(pattern.as_bytes(), name.as_bytes())
}

/// Matches one '[...]' class at the start of p; returns (matched, rest-after-']').
fn match_class(p: &[u8], c: u8) -> Option<(bool, &[u8])> {
    let mut i = 1;
    let mut negate = false;
    if p.get(i) == Some(&b'!') {
        negate = true;
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        if p[i] == b']' && !first {
            return Some((matched != negate, &p[i + 1..]));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            if c >= p[i] && c <= p[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if c == p[i] {
                matched = true;
            }
            i += 1;
        }
    }
    None // unterminated class
}

/// glibc glob() with GLOB_TILDE for "~": $HOME, else the passwd entry.
fn home_dir() -> String {
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() {
            return home;
        }
    }
    getpwuid_home().unwrap_or_default()
}

/// getpwuid(getuid())->pw_dir
fn getpwuid_home() -> Option<String> {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        Some(
            CStr::from_ptr((*pw).pw_dir)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// getpwnam(user)->pw_dir
fn getpwnam_home(user: &str) -> Option<String> {
    let cuser = CString::new(user).ok()?;
    unsafe {
        let pw = libc::getpwnam(cuser.as_ptr());
        if pw.is_null() {
            return None;
        }
        Some(
            CStr::from_ptr((*pw).pw_dir)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::test_util::env_lock()
    }

    #[test]
    fn tilde_resolves_to_home() {
        let _g = lock_env();
        let tmp = std::env::temp_dir();
        std::env::set_var("HOME", &tmp);
        assert_eq!(
            resolve_tilde("~/.config"),
            format!("{}/.config", tmp.to_str().unwrap())
        );
        assert_eq!(resolve_tilde("~"), tmp.to_str().unwrap().to_string());
        // Even a non-existent home directory is returned as the expansion.
        std::env::set_var("HOME", "/nonexistent-home-xyz");
        assert_eq!(resolve_tilde("~/foo"), "/nonexistent-home-xyz/foo");
    }

    #[test]
    fn absolute_path_unchanged() {
        let _g = lock_env();
        // head is empty (path starts with '/') → GLOB_NOMATCH → copy.
        assert_eq!(resolve_tilde("/etc/i3/config"), "/etc/i3/config");
    }

    #[test]
    fn relative_path_unchanged() {
        let _g = lock_env();
        assert_eq!(resolve_tilde("foo/bar"), "foo/bar");
        assert_eq!(resolve_tilde("foo"), "foo");
        assert_eq!(resolve_tilde("no-such-literal-thing/x"), "no-such-literal-thing/x");
    }

    #[test]
    fn unknown_user_unchanged() {
        let _g = lock_env();
        // ~nonexistentuser → glibc returns the unchanged pattern as a match,
        // so the result is a copy of the path.
        assert_eq!(
            resolve_tilde("~nonexistentuser12345/config"),
            "~nonexistentuser12345/config"
        );
    }

    #[test]
    fn fnmatch_cases() {
        assert!(fnmatch_component("i3*", "i3bar"));
        assert!(fnmatch_component("i3?", "i3x"));
        assert!(!fnmatch_component("i3?", "i3xy"));
        assert!(fnmatch_component("i3[ab]", "i3a"));
        assert!(!fnmatch_component("i3[ab]", "i3c"));
        assert!(fnmatch_component("i3[!ab]", "i3c"));
        assert!(!fnmatch_component("i3[!ab]", "i3a"));
        // glob(3) leading-dot rule
        assert!(!fnmatch_component("*", ".hidden"));
        assert!(fnmatch_component(".*", ".hidden"));
    }
}
