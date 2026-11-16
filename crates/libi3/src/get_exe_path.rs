//! C: libi3/get_exe_path.c
use std::ffi::CStr;

use std::env;
use std::ffi::CString;

/// C: libi3/get_exe_path.c:22 — This function returns the absolute path to
/// the executable it is running in.
///
/// The implementation follows https://stackoverflow.com/a/933996/712014
pub fn get_exe_path(argv0: &str) -> String {
    // C: size_t destpath_size = 1024; char *destpath = smalloc(destpath_size);
    let mut destpath_size: usize = 1024;
    // (C: const char *exepath = "/proc/self/exe"; — C literals are
    // NUL-terminated, so we build a CString to pass to readlink.)
    let exepath = CString::new("/proc/self/exe").unwrap();

    // C: while ((linksize = readlink(exepath, destpath, destpath_size)) ==
    //          (ssize_t)destpath_size) { destpath_size *= 2; ... }
    loop {
        let mut destpath = vec![0u8; destpath_size];
        let linksize = unsafe {
            libc::readlink(
                exepath.as_ptr(),
                destpath.as_mut_ptr() as *mut libc::c_char,
                destpath_size,
            )
        };
        if linksize == -1 {
            break;
        }
        if linksize as usize == destpath_size {
            destpath_size *= 2;
            continue;
        }
        // readlink() does not NULL-terminate strings, so we have to.
        // (C returns the raw bytes as a char*; Rust needs UTF-8.)
        return String::from_utf8_lossy(&destpath[..linksize as usize]).into_owned();
    }

    // argv[0] is most likely a full path if it starts with a slash.
    if argv0.starts_with('/') {
        return argv0.to_string();
    }

    // if argv[0] contains a /, prepend the working directory
    if argv0.contains('/') {
        // C: getcwd() with a growing buffer
        let mut tmp_size: usize = 1024;
        loop {
            let mut tmp = vec![0u8; tmp_size];
            let result =
                unsafe { libc::getcwd(tmp.as_mut_ptr() as *mut libc::c_char, tmp_size) };
            if !result.is_null() {
                let cwd = unsafe { CStr::from_ptr(result) }.to_string_lossy().into_owned();
                return format!("{}/{}", cwd, argv0);
            }
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == libc::ERANGE {
                tmp_size *= 2;
                continue;
            }
            break; // C: if (retgcwd != NULL) ... — otherwise fall through
        }
    }

    // Fall back to searching $PATH (or _CS_PATH in absence of $PATH).
    // C: char *path = getenv("PATH");
    let path: String = match env::var("PATH") {
        Ok(p) => p,
        Err(_) => {
            // C: _CS_PATH is typically something like "/bin:/usr/bin"
            // C: while (confstr(_CS_PATH, tmp, tmp_size) > tmp_size) { grow }
            let mut tmp_size: usize = 1024;
            loop {
                let mut tmp = vec![0u8; tmp_size];
                let needed = unsafe {
                    libc::confstr(
                        libc::_CS_PATH,
                        tmp.as_mut_ptr() as *mut libc::c_char,
                        tmp_size,
                    )
                };
                if needed == 0 {
                    return last_resort(); // confstr failed
                }
                if needed > tmp_size {
                    tmp_size = needed;
                    continue;
                }
                // C: sasprintf(&path, ":%s", tmp); (strtok skips the empty
                // first token, so the leading ':' is irrelevant here)
                break String::from_utf8_lossy(&tmp[..needed.saturating_sub(1)]).into_owned();
            }
        }
    };

    // C: strtok(str, ":") — skips empty tokens
    for component in path.split(':').filter(|c| !c.is_empty()) {
        let destpath = format!("{}/{}", component, argv0);
        // Of course this is not 100% equivalent to actually exec()ing the
        // binary, but meh.
        if let Some(cpath) = CString::new(destpath.clone()).ok() {
            if unsafe { libc::access(cpath.as_ptr(), libc::X_OK) } == 0 {
                return destpath;
            }
        }
    }

    // Last resort: maybe it's in /usr/bin?
    last_resort()
}

/// C: return sstrdup("/usr/bin/i3-nagbar");
fn last_resort() -> String {
    "/usr/bin/i3-nagbar".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_path_is_proc_self_exe() {
        // On Linux, readlink("/proc/self/exe") always succeeds, so
        // get_exe_path never consults argv0 here.
        let expected = std::fs::read_link("/proc/self/exe").unwrap();
        let got = get_exe_path("whatever");
        assert_eq!(std::path::Path::new(&got), expected);
    }
}
