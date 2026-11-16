//! C: libi3/create_socket.c

use crate::err::{perror, set_errno};
use crate::ipc_connect::ipc_connect_impl;
use crate::log::{debuglog, errorlog};
use crate::mkdirp::mkdirp;
use crate::nonblock::set_nonblock;
use crate::path_exists::path_exists;
use crate::resolve_tilde::resolve_tilde;
use crate::DEFAULT_DIR_MODE;

/// C: libi3/create_socket.c:28 — Creates the UNIX domain socket at the given
/// path, sets it to non-blocking mode, bind()s and listen()s on it.
///
/// The full path to the socket is stored in the out_socketpath that
/// out_socketpath points to.
///
/// Returns the socket file descriptor, or -1 on error (out_socketpath maps
/// the C char** out parameter; errno is set to EEXIST when the socket is
/// already in use).
pub fn create_socket(filename: &str, out_socketpath: &mut Option<String>) -> i32 {
    let resolved = resolve_tilde(filename);
    // C: DLOG("Creating UNIX socket at %s\n", resolved);
    debuglog(&format!(
        "libi3/create_socket.c:create_socket:30 - Creating UNIX socket at {}\n",
        resolved
    ));
    // C: char *copy = sstrdup(resolved); const char *dir = dirname(copy);
    let dir = dirname(&resolved);
    if !path_exists(&dir) {
        // C: mkdirp(dir, DEFAULT_DIR_MODE);
        mkdirp(&dir, DEFAULT_DIR_MODE);
    }

    // Check if the socket is in use by another process (this call does not
    // succeed if the socket is stale / the owner already exited)
    let sockfd = ipc_connect_impl(&resolved);
    if sockfd != -1 {
        // C: ELOG("Refusing to create UNIX socket at %s: Socket is already
        //         in use\n", resolved);
        errorlog(&format!(
            "[libi3] ERROR: libi3/create_socket.c Refusing to create UNIX socket at {}: Socket is already in use\n",
            resolved
        ));
        unsafe { libc::close(sockfd) };
        set_errno(libc::EEXIST);
        return -1;
    }

    // Unlink the unix domain socket before
    if let Ok(cresolved) = std::ffi::CString::new(resolved.clone()) {
        unsafe { libc::unlink(cresolved.as_ptr()) };
    }

    let sockfd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if sockfd < 0 {
        // C: perror("socket()");
        perror("socket()");
        return -1;
    }

    // C: (void)fcntl(sockfd, F_SETFD, FD_CLOEXEC);
    unsafe {
        libc::fcntl(sockfd, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // C: strncpy(addr.sun_path, resolved, sizeof(addr.sun_path) - 1);
    let bytes = resolved.as_bytes();
    let len = bytes.len().min(addr.sun_path.len() - 1);
    for (d, s) in addr.sun_path[..len].iter_mut().zip(bytes) {
        *d = *s as libc::c_char;
    }
    if unsafe {
        libc::bind(
            sockfd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    } < 0
    {
        // C: perror("bind()");
        perror("bind()");
        return -1;
    }

    set_nonblock(sockfd);

    if unsafe { libc::listen(sockfd, 5) } < 0 {
        // C: perror("listen()");
        perror("listen()");
        return -1;
    }

    // C: free(*out_socketpath); *out_socketpath = resolved;
    *out_socketpath = Some(resolved);
    sockfd
}

/// C: <libgen.h> dirname() — POSIX semantics (modifies a copy of the path).
fn dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let mut copy = path.to_string();
    // strip trailing slashes, if any
    while copy.ends_with('/') {
        copy.pop();
    }
    if copy.is_empty() {
        // the path consisted of slashes only
        return "/".to_string();
    }
    match copy.rfind('/') {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(pos) => copy[..pos].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_socket_binds_and_listens() {
        let tmp = std::env::temp_dir().join(format!("i3rs-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = tmp.join("ipc-socket.sock");
        let mut out_socketpath: Option<String> = None;
        let fd = create_socket(path.to_str().unwrap(), &mut out_socketpath);
        assert!(fd >= 0, "create_socket failed");
        assert_eq!(out_socketpath, Some(path.to_str().unwrap().to_string()));
        // the socket file exists and accepts connections
        assert!(path_exists(path.to_str().unwrap()));
        use std::os::unix::net::UnixStream;
        let client = UnixStream::connect(&path).expect("connect to created socket");
        drop(client);
        unsafe { libc::close(fd) };
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn create_socket_refuses_when_in_use() {
        let tmp = std::env::temp_dir().join(format!("i3rs-sock2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = tmp.join("ipc-socket.sock");
        let mut out: Option<String> = None;
        let fd = create_socket(path.to_str().unwrap(), &mut out);
        assert!(fd >= 0);
        // second create_socket on the live socket → EEXIST
        let mut out2: Option<String> = None;
        let fd2 = create_socket(path.to_str().unwrap(), &mut out2);
        assert_eq!(fd2, -1);
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        assert_eq!(errno, libc::EEXIST);
        assert_eq!(out2, None);
        unsafe { libc::close(fd) };
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dirname_cases() {
        assert_eq!(dirname("/a/b/c"), "/a/b");
        assert_eq!(dirname("/a/b/"), "/a");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("a/b"), "a");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("/"), "/");
        assert_eq!(dirname("///"), "/");
        assert_eq!(dirname(""), ".");
    }
}
