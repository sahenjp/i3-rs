//! C: libi3/safewrappers.c — the s* (safe) wrappers.
//!
//! smalloc/scalloc/srealloc/sstrdup/sstrndup/sasprintf are intentionally NOT
//! ported: their whole purpose is aborting the process on allocation failure,
//! which Rust's Box/Vec/String and format! already do by default. The
//! fd-based helpers (writeall, writeall_nonblock, swrite) and
//! strcasecmp_nullable are ported below.

use crate::err::err;

fn errno_value() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// C: <strings.h> strcasecmp() — byte-wise case-insensitive comparison
/// (ASCII tolower in the C locale); returns <0, 0 or >0 like strcmp.
pub fn strcasecmp(a: &str, b: &str) -> i32 {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let len = a.len().max(b.len());
    for i in 0..len {
        let ca = a.get(i).copied().unwrap_or(0).to_ascii_lowercase() as i32;
        let cb = b.get(i).copied().unwrap_or(0).to_ascii_lowercase() as i32;
        if ca != cb {
            return ca - cb;
        }
    }
    0
}

/// C: libi3/safewrappers.c:73 writeall — write() loop retrying on
/// EINTR/EAGAIN. Returns -1 when write fails (errno remains) or the number of
/// bytes written. In Rust the slice length is the C (buf, count) pair.
pub fn writeall(fd: i32, buf: &[u8]) -> isize {
    let mut written: usize = 0;
    let count = buf.len();

    while written < count {
        let n = unsafe {
            libc::write(fd, buf.as_ptr().add(written) as *const libc::c_void, count - written)
        };
        if n == -1 {
            let errno = errno_value();
            if errno == libc::EINTR || errno == libc::EAGAIN {
                continue;
            }
            return n;
        }
        written += n as usize;
    }

    written as isize
}

/// C: libi3/safewrappers.c:90 writeall_nonblock — like writeall, but stops
/// (returning the total so far) when a write would block (EAGAIN).
pub fn writeall_nonblock(fd: i32, buf: &[u8]) -> isize {
    let mut written: usize = 0;
    let count = buf.len();

    while written < count {
        let n = unsafe {
            libc::write(fd, buf.as_ptr().add(written) as *const libc::c_void, count - written)
        };
        if n == -1 {
            let errno = errno_value();
            if errno == libc::EAGAIN {
                return written as isize;
            } else if errno == libc::EINTR {
                continue;
            } else {
                return n;
            }
        }
        written += n as usize;
    }

    written as isize
}

/// C: libi3/safewrappers.c:109 swrite — writeall that dies on failure.
/// (The C message formats the file descriptor, kept 1:1.)
pub fn swrite(fd: i32, buf: &[u8]) -> isize {
    let n = writeall(fd, buf);
    if n == -1 {
        err(1, &format!("Failed to write {}", fd));
    }
    n
}

/// C: libi3/safewrappers.c:124 strcasecmp_nullable — like strcasecmp but
/// considers the case where either string is NULL.
pub fn strcasecmp_nullable(a: Option<&str>, b: Option<&str>) -> i32 {
    // C: if (a == b) return 0 — both NULL (or the same pointer).
    if a.is_none() && b.is_none() {
        return 0;
    }
    if a.is_none() {
        return -1;
    }
    if b.is_none() {
        return 1;
    }
    strcasecmp(a.unwrap(), b.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strcasecmp_basic() {
        assert_eq!(strcasecmp("COMMAND", "command"), 0);
        assert_eq!(strcasecmp("YES", "yes"), 0);
        assert_eq!(strcasecmp("a", "b") < 0, true);
        assert_eq!(strcasecmp("b", "a") > 0, true);
        assert_eq!(strcasecmp("ab", "abc") < 0, true);
    }

    #[test]
    fn strcasecmp_nullable_cases() {
        assert_eq!(strcasecmp_nullable(None, None), 0);
        assert_eq!(strcasecmp_nullable(None, Some("x")), -1);
        assert_eq!(strcasecmp_nullable(Some("x"), None), 1);
        assert_eq!(strcasecmp_nullable(Some("X"), Some("x")), 0);
    }

    #[test]
    fn writeall_bad_fd() {
        // write() on a closed fd fails with EBADF → -1, errno remains.
        assert_eq!(writeall(-1, b"x"), -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn writeall_to_pipe() {
        use std::os::unix::io::AsRawFd;
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert_eq!(writeall(fds[1], b"hello"), 5);
        let mut buf = [0u8; 5];
        assert_eq!(unsafe { libc::read(fds[0], buf.as_mut_ptr() as *mut libc::c_void, 5) }, 5);
        assert_eq!(&buf, b"hello");
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
