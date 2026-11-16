//! C: libi3/ipc_connect.c

use crate::err::err;
use crate::root_atom_contents::root_atom_contents;
use std::env;

/// C: libi3/ipc_connect.c:23 — Connects to the i3 IPC socket and returns the
/// file descriptor for the socket. die()s if anything goes wrong.
///
/// Falls back to $I3SOCK, then the I3_SOCKET_PATH root window atom.
pub fn ipc_connect(socket_path: Option<&str>) -> i32 {
    let mut path: Option<String> = None;
    if let Some(sp) = socket_path {
        path = Some(sp.to_string());
    }

    if path.is_none() {
        if let Ok(sock) = env::var("I3SOCK") {
            path = Some(sock);
        }
    }

    if path.is_none() {
        path = root_atom_contents("I3_SOCKET_PATH", None, 0);
    }

    let path = match path {
        Some(p) => p,
        None => err(1, "Could not determine i3 socket path"),
    };

    let sockfd = ipc_connect_impl(&path);
    if sockfd < 0 {
        err(1, &format!("Could not connect to i3 on socket {}", path));
    }
    sockfd
}

/// C: libi3/ipc_connect.c:56 — Connects to the socket at the given path with
/// no fallback paths. Returns -1 if connect() fails and die()s for other
/// errors.
pub fn ipc_connect_impl(socket_path: &str) -> i32 {
    // C: int sockfd = socket(AF_LOCAL, SOCK_STREAM, 0);
    let sockfd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if sockfd == -1 {
        err(1, "Could not create socket");
    }

    // C: (void)fcntl(sockfd, F_SETFD, FD_CLOEXEC);
    unsafe {
        libc::fcntl(sockfd, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    // C: struct sockaddr_un addr; memset(&addr, 0, sizeof(addr));
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // C: strncpy(addr.sun_path, socket_path, sizeof(addr.sun_path) - 1);
    let bytes = socket_path.as_bytes();
    let len = bytes.len().min(addr.sun_path.len() - 1);
    for (d, s) in addr.sun_path[..len].iter_mut().zip(bytes) {
        *d = *s as libc::c_char;
    }
    if unsafe {
        libc::connect(
            sockfd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    } < 0
    {
        unsafe { libc::close(sockfd) };
        return -1;
    }
    sockfd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn ipc_connect_impl_connects() {
        let path = std::env::temp_dir().join(format!("i3rs-ipc-test-{}.sock", std::process::id()));
        let _cleanup = Cleanup(path.clone());
        let listener = UnixListener::bind(&path).expect("bind");
        let fd = ipc_connect_impl(path.to_str().unwrap());
        assert!(fd >= 0);
        let (_conn, _addr) = listener.accept().expect("accept");
        unsafe { libc::close(fd) };
    }

    #[test]
    fn ipc_connect_impl_refused() {
        // No listener at this path → connect() fails → -1.
        let path = format!("/tmp/i3rs-ipc-refused-{}.sock", std::process::id());
        assert_eq!(ipc_connect_impl(&path), -1);
    }

    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}
