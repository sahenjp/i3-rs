//! C: libi3/ipc_recv_message.c

use crate::ipc::I3_IPC_MAGIC;
use crate::log::errorlog;

fn errno_value() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// C: libi3/ipc_recv_message.c:29 — Reads a message from the given socket
/// file descriptor and stores its length (reply_length) as well as a pointer
/// to its contents (reply).
///
/// Returns -1 when read() fails, errno will remain.
/// Returns -2 on EOF.
/// Returns -3 when the IPC protocol is violated (invalid magic, unexpected
/// message type, EOF instead of a message). Additionally, the error will be
/// printed to stderr.
/// Returns 0 on success.
///
/// message_type maps the C uint32_t* (may be NULL → not stored);
/// reply maps the C uint8_t** out parameter.
pub fn ipc_recv_message(
    sockfd: i32,
    message_type: Option<&mut u32>,
    reply_length: &mut u32,
    reply: &mut Vec<u8>,
) -> i32 {
    // Read the message header first
    // C: const uint32_t to_read = strlen(I3_IPC_MAGIC) + 2 * sizeof(uint32_t);
    let to_read: u32 = I3_IPC_MAGIC.len() as u32 + 4 + 4;
    let mut msg = [0u8; 14];

    let mut read_bytes: u32 = 0;
    while read_bytes < to_read {
        // Note: the C header loop does NOT retry on EINTR/EAGAIN (unlike the
        // payload loop below) — kept 1:1.
        let n = unsafe {
            libc::read(
                sockfd,
                msg.as_mut_ptr().add(read_bytes as usize) as *mut libc::c_void,
                (to_read - read_bytes) as usize,
            )
        };
        if n == -1 {
            return -1;
        }
        if n == 0 {
            if read_bytes == 0 {
                return -2;
            } else {
                // C: ELOG("IPC: unexpected EOF while reading header, got %u
                //         bytes, want %u bytes")
                errorlog(&format!(
                    "[libi3] ERROR: libi3/ipc_recv_message.c IPC: unexpected EOF while reading header, got {} bytes, want {} bytes\n",
                    read_bytes, to_read
                ));
                return -3;
            }
        }

        read_bytes += n as u32;
    }

    // C: if (memcmp(walk, I3_IPC_MAGIC, strlen(I3_IPC_MAGIC)) != 0)
    if &msg[0..6] != I3_IPC_MAGIC.as_bytes() {
        // C: ELOG("IPC: invalid magic in header, got \"%.*s\", want \"%s\"")
        errorlog(&format!(
            "[libi3] ERROR: libi3/ipc_recv_message.c IPC: invalid magic in header, got \"{}\", want \"{}\"\n",
            String::from_utf8_lossy(&msg[0..6]),
            I3_IPC_MAGIC
        ));
        return -3;
    }

    // C: walk += strlen(I3_IPC_MAGIC); memcpy(reply_length, walk, 4); ...
    *reply_length = u32::from_ne_bytes(msg[6..10].try_into().unwrap());
    if let Some(mt) = message_type {
        *mt = u32::from_ne_bytes(msg[10..14].try_into().unwrap());
    }

    // C: *reply = smalloc(*reply_length);
    reply.clear();
    reply.resize(*reply_length as usize, 0);

    read_bytes = 0;
    while (read_bytes as usize) < *reply_length as usize {
        let n = unsafe {
            libc::read(
                sockfd,
                reply.as_mut_ptr().add(read_bytes as usize) as *mut libc::c_void,
                *reply_length as usize - read_bytes as usize,
            )
        };
        if n == -1 {
            let errno = errno_value();
            if errno == libc::EINTR || errno == libc::EAGAIN {
                continue;
            }
            return -1;
        }
        if n == 0 {
            // C: ELOG("IPC: unexpected EOF while reading payload, got %u
            //         bytes, want %u bytes")
            errorlog(&format!(
                "[libi3] ERROR: libi3/ipc_recv_message.c IPC: unexpected EOF while reading payload, got {} bytes, want {} bytes\n",
                read_bytes, *reply_length
            ));
            return -3;
        }

        read_bytes += n as u32;
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    fn read_message(stream: &mut UnixStream) -> (u32, Vec<u8>) {
        let mut header = [0u8; 14];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..6], b"i3-ipc");
        let size = u32::from_ne_bytes(header[6..10].try_into().unwrap());
        let type_ = u32::from_ne_bytes(header[10..14].try_into().unwrap());
        let mut payload = vec![0u8; size as usize];
        stream.read_exact(&mut payload).unwrap();
        (type_, payload)
    }

    fn write_message(stream: &mut UnixStream, type_: u32, payload: &[u8]) {
        let mut msg = Vec::new();
        msg.extend_from_slice(b"i3-ipc");
        msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        msg.extend_from_slice(&type_.to_ne_bytes());
        msg.extend_from_slice(payload);
        stream.write_all(&msg).unwrap();
    }

    #[test]
    fn ipc_roundtrip() {
        let (client, server) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut server = server;
            let (type_, payload) = read_message(&mut server);
            assert_eq!(type_, 0);
            assert_eq!(payload, b"nop");
            write_message(&mut server, 4, br#"{"id":1}"#);
        });
        let fd = client.as_raw_fd();
        assert_eq!(crate::ipc_send_message::ipc_send_message(fd, 3, 0, b"nop"), 0);

        let mut reply_type = 0u32;
        let mut reply_length = 0u32;
        let mut reply: Vec<u8> = Vec::new();
        assert_eq!(
            ipc_recv_message(fd, Some(&mut reply_type), &mut reply_length, &mut reply),
            0
        );
        assert_eq!(reply_type, 4);
        assert_eq!(reply_length, 8);
        assert_eq!(reply, br#"{"id":1}"#);
        server.join().unwrap();
    }

    #[test]
    fn ipc_recv_eof_returns_minus_2() {
        let (client, server) = UnixStream::pair().unwrap();
        drop(server);
        let fd = client.as_raw_fd();
        let mut reply_type = 0u32;
        let mut reply_length = 0u32;
        let mut reply: Vec<u8> = Vec::new();
        assert_eq!(
            ipc_recv_message(fd, Some(&mut reply_type), &mut reply_length, &mut reply),
            -2
        );
    }

    #[test]
    fn ipc_recv_invalid_magic_returns_minus_3() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"XXXXXXXXXXXXXX").unwrap();
        drop(server);
        let fd = client.as_raw_fd();
        let mut reply_type = 0u32;
        let mut reply_length = 0u32;
        let mut reply: Vec<u8> = Vec::new();
        assert_eq!(
            ipc_recv_message(fd, None, &mut reply_length, &mut reply),
            -3
        );
    }

    #[test]
    fn ipc_recv_partial_header_eof_returns_minus_3() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"i3-ip").unwrap();
        drop(server);
        let fd = client.as_raw_fd();
        let mut reply_length = 0u32;
        let mut reply: Vec<u8> = Vec::new();
        assert_eq!(ipc_recv_message(fd, None, &mut reply_length, &mut reply), -3);
    }

    #[test]
    fn ipc_recv_partial_payload_eof_returns_minus_3() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut header = Vec::new();
        header.extend_from_slice(b"i3-ipc");
        header.extend_from_slice(&10u32.to_ne_bytes());
        header.extend_from_slice(&0u32.to_ne_bytes());
        server.write_all(&header).unwrap();
        server.write_all(b"only4").unwrap();
        drop(server);
        let fd = client.as_raw_fd();
        let mut reply_length = 0u32;
        let mut reply: Vec<u8> = Vec::new();
        assert_eq!(ipc_recv_message(fd, None, &mut reply_length, &mut reply), -3);
    }

    #[test]
    fn ipc_recv_empty_payload() {
        let (client, mut server) = UnixStream::pair().unwrap();
        write_message(&mut server, 7, b"");
        let fd = client.as_raw_fd();
        let mut reply_type = 0u32;
        let mut reply_length = 0u32;
        let mut reply: Vec<u8> = Vec::new();
        assert_eq!(
            ipc_recv_message(fd, Some(&mut reply_type), &mut reply_length, &mut reply),
            0
        );
        assert_eq!(reply_type, 7);
        assert_eq!(reply_length, 0);
        assert!(reply.is_empty());
    }

    #[test]
    fn ipc_recv_bad_fd_returns_minus_1() {
        let mut reply_length = 0u32;
        let mut reply: Vec<u8> = Vec::new();
        assert_eq!(ipc_recv_message(-1, None, &mut reply_length, &mut reply), -1);
    }
}
