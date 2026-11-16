//! Port of the parts of C: src/sd-daemon.c that i3 uses.
//!
//! i3 ships a trimmed copy of systemd's sd-daemon.c in C (mainly for the
//! socket activation helper), so the port mirrors the same subset:
//! `sd_listen_fds` (src/sd-daemon.c:47-121) and the constant it relies on.

/// C: include/sd-daemon.h:102 #define SD_LISTEN_FDS_START 3
pub const SD_LISTEN_FDS_START: i32 = 3;

/// C: src/sd-daemon.c:47 int sd_listen_fds(const int unset_environment)
///
/// Returns the number of passed descriptors (>= 0) or a negative errno,
/// exactly like the C version. On success the descriptors themselves are
/// marked FD_CLOEXEC (the caller in main.c then clears it again so an
/// in-place restart can keep them).
pub fn sd_listen_fds(unset_environment: bool) -> i32 {
    let result = (|| -> i32 {
        let pid_env = match std::env::var("LISTEN_PID") {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let pid: i64 = match pid_env.parse::<i64>() {
            Ok(v) => v,
            Err(_) => return -libc::EINVAL,
        };
        if pid <= 0 {
            return -libc::EINVAL;
        }
        // Is this for us?
        if unsafe { libc::getpid() } as i64 != pid {
            return 0;
        }
        let count_env = match std::env::var("LISTEN_FDS") {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let count: i64 = match count_env.parse::<i64>() {
            Ok(v) => v,
            Err(_) => return -libc::EINVAL,
        };
        for fd in SD_LISTEN_FDS_START..SD_LISTEN_FDS_START + count as i32 {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 {
                return -std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EINVAL);
            }
            if flags & libc::FD_CLOEXEC != 0 {
                continue;
            }
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                return -std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EINVAL);
            }
        }
        count as i32
    })();

    if unset_environment {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
    }
    result
}
