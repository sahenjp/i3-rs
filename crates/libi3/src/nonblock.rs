//! C: libi3/nonblock.c

use crate::err::err;

/// C: libi3/nonblock.c:12 — Puts the given socket file descriptor into
/// non-blocking mode or dies if setting O_NONBLOCK failed. Non-blocking
/// sockets are a good idea for our IPC model because we should by no means
/// block the window manager.
pub fn set_nonblock(sockfd: i32) {
    // C: int flags = fcntl(sockfd, F_GETFL, 0);
    let flags: i32 = unsafe { libc::fcntl(sockfd, libc::F_GETFL) };
    // C: a failed fcntl() returns -1, and (-1 & O_NONBLOCK) != 0 makes
    // the C function return early — kept 1:1.
    if flags & libc::O_NONBLOCK != 0 {
        return;
    }
    let flags = flags | libc::O_NONBLOCK;
    // C: if (fcntl(sockfd, F_SETFL, flags) < 0) err(-1, "Could not set O_NONBLOCK");
    if unsafe { libc::fcntl(sockfd, libc::F_SETFL, flags) } < 0
    {
        // C: err(-1, ...) — exits with status 255 (exit(-1) & 0xff).
        err(-1, "Could not set O_NONBLOCK");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_and_keeps_nonblock() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        set_nonblock(fds[0]);
        let flags = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };
        assert!(flags & libc::O_NONBLOCK != 0);
        // idempotent
        set_nonblock(fds[0]);
        let flags = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };
        assert!(flags & libc::O_NONBLOCK != 0);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
