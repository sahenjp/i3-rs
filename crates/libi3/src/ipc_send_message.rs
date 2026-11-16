//! C: libi3/ipc_send_message.c

use crate::safewrappers::writeall;

/// C: libi3/ipc_send_message.c:21 — Formats a message (payload) of the given
/// size and type and sends it to i3 via the given socket file descriptor.
///
/// Returns -1 when write() fails, errno will remain. Returns 0 on success.
///
/// Header: i3_ipc_header_t (C: include/i3/ipc.h:15) — "i3-ipc" magic (6
/// bytes) + u32 size + u32 type, packed, native endianness.
pub fn ipc_send_message(sockfd: i32, message_size: u32, message_type: u32, payload: &[u8]) -> i32 {
    // C: const i3_ipc_header_t header = { .magic = {'i','3','-','i','p','c'},
    //                                     .size = message_size, .type = message_type };
    let mut header = [0u8; 14];
    header[0..6].copy_from_slice(b"i3-ipc");
    header[6..10].copy_from_slice(&message_size.to_ne_bytes());
    header[10..14].copy_from_slice(&message_type.to_ne_bytes());

    if writeall(sockfd, &header) == -1 {
        return -1;
    }

    // C: writeall(sockfd, payload, message_size). (C trusts message_size to
    // be within the payload buffer; the slice panics on overflow instead of
    // reading out of bounds.)
    let size = message_size as usize;
    if writeall(sockfd, &payload[..size]) == -1 {
        return -1;
    }

    0
}
