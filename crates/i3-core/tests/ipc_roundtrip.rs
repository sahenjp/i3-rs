//! Integration check: the IPC server answers a real GET_VERSION frame.
//! C: src/ipc.c:1444 ipc_receive_message + handlers[]. The socket lives in a
//! temp dir, the client is a plain UnixStream.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use i3_core::backend::NullBackend;
use i3_core::ipc::{handle_readable, IpcServer};

fn recv_frame(stream: &mut UnixStream) -> (u32, Vec<u8>) {
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).expect("header");
    assert_eq!(&header[..6], b"i3-ipc");
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]);
    let ty = u32::from_ne_bytes([header[10], header[11], header[12], header[13]]);
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        stream.read_exact(&mut payload).expect("payload");
    }
    (ty, payload)
}

fn send_frame(stream: &mut UnixStream, ty: u32, payload: &[u8]) {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"i3-ipc");
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&ty.to_ne_bytes());
    msg.extend_from_slice(payload);
    stream.write_all(&msg).expect("write");
}

#[test]
fn get_version_roundtrip_over_a_real_socket() {
    let dir = std::env::temp_dir().join(format!("i3-ipc-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sock");
    let _ = std::fs::remove_file(&path);
    let mut server = IpcServer::bind(path.to_str().unwrap()).unwrap();

    let mut client = UnixStream::connect(&path).unwrap();
    client.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();

    // C: the ev_io accept watcher
    assert!(server.accept_client().is_some(), "accept");

    // C: I3_IPC_MESSAGE_TYPE_GET_VERSION == 7
    send_frame(&mut client, 7, b"");
    let mut tree = i3_core::Tree::new();
    let mut backend = NullBackend;
    // handle_readable returns -1 (EAGAIN) until the bytes have arrived, so
    // retry like the event loop does.
    let mut replied = false;
    for _ in 0..200 {
        handle_readable(&mut server, 0, &mut tree, &mut backend);
        if server.clients[0].buffer.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if let Ok((ty, payload)) = recv_frame_ready(&mut client) {
            assert_eq!(ty, 7, "reply type must be GET_VERSION");
            let text = String::from_utf8_lossy(&payload);
            assert!(text.contains("human_readable"), "reply was {}", text);
            replied = true;
            break;
        }
    }
    assert!(replied, "no IPC reply within the retry budget");
}

/// Non-blocking peek for the test: returns Ok when a full frame was read.
fn recv_frame_ready(stream: &mut UnixStream) -> Result<(u32, Vec<u8>), std::io::Error> {
    stream.set_nonblocking(true)?;
    let mut header = [0u8; 14];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) => {
            stream.set_nonblocking(false)?;
            return Err(e);
        }
    }
    stream.set_nonblocking(false)?;
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]);
    let ty = u32::from_ne_bytes([header[10], header[11], header[12], header[13]]);
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        stream.read_exact(&mut payload)?;
    }
    Ok((ty, payload))
}