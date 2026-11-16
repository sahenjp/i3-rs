//! C: libi3/mkdirp.c

use crate::err::strerror;
use crate::log::errorlog;
use std::ffi::CString;

fn errno_value() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// C: libi3/mkdirp.c:20 — Emulates mkdir -p (creates any missing folders).
/// Returns 0 on success, -1 on failure.
pub fn mkdirp(path: &str, mode: u32) -> i32 {
    let cpath = CString::new(path).unwrap_or_default();
    if unsafe { libc::mkdir(cpath.as_ptr(), mode) } == 0 {
        return 0;
    }
    let errno = errno_value();
    if errno == libc::EEXIST {
        // Check that the named file actually is a directory.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::stat(cpath.as_ptr(), &mut st) } != 0 {
            // C: ELOG("stat(%s) failed: %s\n", path, strerror(errno))
            errorlog(&format!(
                "[libi3] ERROR: libi3/mkdirp.c stat({}) failed: {}\n",
                path,
                strerror(errno)
            ));
            return -1;
        }
        if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            // C: ELOG("mkdir(%s) failed: %s\n", path, strerror(ENOTDIR))
            errorlog(&format!(
                "[libi3] ERROR: libi3/mkdirp.c mkdir({}) failed: {}\n",
                path,
                strerror(libc::ENOTDIR)
            ));
            return -1;
        }
        return 0;
    } else if errno != libc::ENOENT {
        // C: ELOG("mkdir(%s) failed: %s\n", path, strerror(errno))
        errorlog(&format!(
            "[libi3] ERROR: libi3/mkdirp.c mkdir({}) failed: {}\n",
            path,
            strerror(errno)
        ));
        return -1;
    }

    // C: char *copy = sstrdup(path); strip trailing slashes
    let mut copy = path.to_string();
    while copy.ends_with('/') {
        copy.pop();
    }
    // (C indexes copy[strlen-1]; an empty copy would read out of bounds —
    // unreachable here since mkdir("") yields ENOENT and rfind returns None.)

    // C: char *sep = strrchr(copy, '/'); if (sep == NULL) { free; return -1; }
    let sep = match copy.rfind('/') {
        None => return -1,
        Some(pos) => pos,
    };
    // C: *sep = '\0'; if (mkdirp(copy, mode) == 0) result = mkdirp(path, mode);
    let parent = &copy[..sep];
    let mut result = -1;
    if mkdirp(parent, mode) == 0 {
        result = mkdirp(path, mode);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!("i3rs-mkdirp-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> String {
            self.0.to_str().unwrap().to_string()
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_missing_folders() {
        let tmp = TempDir::new("nested");
        let target = format!("{}/a/b/c", tmp.path());
        assert_eq!(mkdirp(&target, 0o700), 0);
        assert!(crate::path_exists::path_exists(&target));
    }

    #[test]
    fn existing_directory_is_ok() {
        let tmp = TempDir::new("exists");
        assert_eq!(mkdirp(&tmp.path(), 0o700), 0);
        // trailing slashes are stripped
        assert_eq!(mkdirp(&format!("{}/", tmp.path()), 0o700), 0);
    }

    #[test]
    fn existing_file_fails() {
        let tmp = TempDir::new("file");
        let file = format!("{}/some-file", tmp.path());
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(mkdirp(&file, 0o700), -1);
    }

    #[test]
    fn relative_single_component_succeeds() {
        // C: mkdir("relative-name", mode) == 0 → return 0 (no recursion)
        let name = format!("i3-mkdirp-rel-{}", std::process::id());
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(std::env::temp_dir()).unwrap();
        assert_eq!(mkdirp(&name, 0o700), 0);
        std::env::set_current_dir(cwd).unwrap();
        let _ = std::fs::remove_dir(std::env::temp_dir().join(&name));
    }
}
