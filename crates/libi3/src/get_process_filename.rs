//! C: libi3/get_process_filename.c

use crate::err::{errx, perror, warn};
use crate::mkdirp::mkdirp;
use std::env;
use std::ffi::CStr;
use std::sync::Mutex;

/// C: libi3/get_process_filename.c:17 static char *dir
/// (C: stores the directory path for this and all subsequent calls so that
/// we only create a temporary directory once per i3 instance.)
static DIR: Mutex<Option<String>> = Mutex::new(None);

/// C: libi3/get_process_filename.c:20 — Returns the name of a temporary file
/// with the specified prefix.
pub fn get_process_filename(prefix: &str) -> Option<String> {
    let mut dir = DIR.lock().unwrap_or_else(|e| e.into_inner());

    if dir.is_none() {
        // Check if XDG_RUNTIME_DIR is set. If so, we use XDG_RUNTIME_DIR/i3
        if let Ok(xdg) = env::var("XDG_RUNTIME_DIR") {
            let d = format!("{}/i3", xdg);
            // mkdirp() should prevent race between multiple i3 instances
            // started in parallel from causing problem
            if mkdirp(&d, 0o700) == -1 {
                // C: warn("Could not mkdirp(%s)", dir);
                warn(&format!("Could not mkdirp({})", d));
                // C: errx(EXIT_FAILURE, "Check permissions of $XDG_RUNTIME_DIR = '%s'", ...)
                // (the C perror("mkdirp()") and return NULL after errx are
                // unreachable — errx exits.)
                errx(
                    1,
                    &format!("Check permissions of $XDG_RUNTIME_DIR = '{}'", xdg),
                );
            }
            *dir = Some(d);
        } else {
            // If not, we create a (secure) temp directory using the template
            // /tmp/i3-<user>.XXXXXX
            let username = current_username();
            let d = format!("/tmp/i3-{}.XXXXXX", username);
            // C: mkdtemp modifies dir
            let mut buf: Vec<u8> = d.clone().into_bytes();
            buf.push(b'\0');
            let result = unsafe { libc::mkdtemp(buf.as_mut_ptr() as *mut libc::c_char) };
            if result.is_null() {
                // C: perror("mkdtemp()"); return NULL;
                perror("mkdtemp()");
                // C bug kept 1:1: the static dir already holds the template
                // (mkdtemp failed), so subsequent calls keep using it.
                *dir = Some(d);
                return None;
            }
            *dir = Some(
                CStr::from_bytes_with_nul(&buf)
                    .expect("mkdtemp NUL-terminates")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    // C: sasprintf(&filename, "%s/%s.%d", dir, prefix, getpid());
    let dir = dir.as_ref().expect("dir was just set");
    Some(format!("{}/{}.{}", dir, prefix, std::process::id()))
}

/// C: struct passwd *pw = getpwuid(getuid()); const char *username =
/// pw ? pw->pw_name : "unknown";
fn current_username() -> String {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return "unknown".to_string();
        }
        CStr::from_ptr((*pw).pw_name)
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::test_util::env_lock()
    }

    #[test]
    fn xdg_runtime_dir_is_used_once() {
        let _g = lock_env();
        let tmp = std::env::temp_dir().join(format!("i3rs-proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", tmp.to_str().unwrap());

        let a = get_process_filename("prefix-a").unwrap();
        let b = get_process_filename("prefix-b").unwrap();
        let expected_dir = format!("{}/i3", tmp.to_str().unwrap());
        // the XDG_RUNTIME_DIR/i3 directory is created
        assert!(crate::path_exists::path_exists(&expected_dir));
        assert_eq!(
            a,
            format!("{}/prefix-a.{}", expected_dir, std::process::id())
        );
        assert_eq!(
            b,
            format!("{}/prefix-b.{}", expected_dir, std::process::id())
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
