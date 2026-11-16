//! C: libi3/path_exists.c

use std::ffi::CString;

/// C: libi3/path_exists.c:18 — Checks if the given path exists by calling
/// stat().
pub fn path_exists(path: &str) -> bool {
    let mut buf: libc::stat = unsafe { std::mem::zeroed() };
    // (A path with an interior NUL byte would be truncated by C's stat();
    // the empty path simply does not exist.)
    match CString::new(path) {
        Ok(cpath) => unsafe { libc::stat(cpath.as_ptr(), &mut buf) == 0 },
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_paths() {
        assert!(path_exists("/"));
        assert!(path_exists("/etc"));
        let tmp = std::env::temp_dir().join(format!("i3rs-exists-{}", std::process::id()));
        std::fs::write(&tmp, b"x").unwrap();
        assert!(path_exists(tmp.to_str().unwrap()));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn missing_paths() {
        assert!(!path_exists("/no/such/path/xyz"));
        assert!(!path_exists(""));
    }
}
