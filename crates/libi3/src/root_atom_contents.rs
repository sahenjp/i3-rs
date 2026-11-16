//! C: libi3/root_atom_contents.c

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
use x11rb::rust_connection::RustConnection;

/// C: libi3/root_atom_contents.c:28 — Try to get the contents of the given
/// atom (for example I3_SOCKET_PATH) from the X11 root window and return NULL
/// if it doesn't work.
///
/// If the provided XCB connection is None, a new connection will be
/// established. The xcb_connection_t* type maps to x11rb's RustConnection in
/// this port.
pub fn root_atom_contents(atomname: &str, provided_conn: Option<&RustConnection>, screen: i32) -> Option<String> {
    let mut screen = screen;

    // C: if (provided_conn == NULL && ((conn = xcb_connect(NULL, &screen)) == NULL ||
    //                                   xcb_connection_has_error(conn))) return NULL;
    // (RustConnection::connect parses $DISPLAY like xcb_connect(NULL, ...).)
    let own_conn;
    let conn: &RustConnection = match provided_conn {
        Some(c) => c,
        None => match RustConnection::connect(None) {
            Ok((c, s)) => {
                screen = s as i32;
                own_conn = c;
                &own_conn
            }
            Err(_) => return None,
        },
    };

    let result = root_atom_contents_on(conn, atomname, screen);

    // C: if (provided_conn == NULL) xcb_disconnect(conn);
    // (own_conn is dropped here when we established it ourselves.)
    result
}

/// The body of C: libi3/root_atom_contents.c:28 after the connection setup,
/// structured as a function because Rust has no goto for the C cleanup paths.
fn root_atom_contents_on(conn: &RustConnection, atomname: &str, screen: i32) -> Option<String> {
    // C: atom_cookie = xcb_intern_atom(conn, 0, strlen(atomname), atomname);
    let atom_cookie = conn.intern_atom(false, atomname.as_bytes());

    // C: xcb_screen_t *root_screen = xcb_aux_get_screen(conn, screen);
    let root_screen = conn.setup().roots.get(screen as usize)?;
    // C: xcb_window_t root = root_screen->root;
    let root = root_screen.root;

    // C: atom_reply = xcb_intern_atom_reply(conn, atom_cookie, NULL);
    let atom_reply = atom_cookie.ok()?.reply().ok()?;
    let atom = atom_reply.atom;

    // C: content_max_words = 256; get_property(type ANY, offset 0)
    let mut content_max_words: u32 = 256;
    // C: prop_cookie = xcb_get_property_unchecked(conn, false, root, atom,
    //     XCB_GET_PROPERTY_TYPE_ANY, 0, content_max_words);
    let mut prop_reply = conn
        .get_property(false, root, atom, AtomEnum::ANY, 0, content_max_words)
        .ok()?
        .reply()
        .ok()?;

    if !prop_reply.value.is_empty() && prop_reply.bytes_after > 0 {
        // We received an incomplete value. Ask again but with a properly
        // adjusted size.
        // C: content_max_words += ceil(prop_reply->bytes_after / 4.0);
        content_max_words += prop_reply.bytes_after.div_ceil(4);
        // Repeat the request, with adjusted size
        prop_reply = conn
            .get_property(false, root, atom, AtomEnum::ANY, 0, content_max_words)
            .ok()?
            .reply()
            .ok()?;
    }

    // C: if (xcb_get_property_value_length(prop_reply) == 0) goto out;
    if prop_reply.value.is_empty() {
        return None;
    }

    if prop_reply.type_ == u32::from(AtomEnum::CARDINAL) {
        // We treat a CARDINAL as a >= 32-bit unsigned int. The only CARDINAL
        // we query is I3_PID, which is 32-bit.
        // C: sasprintf(&content, "%u", *((unsigned int *)value));
        // (C reads 4 bytes unconditionally; a short value returns None here.)
        let v = prop_reply.value.first_chunk::<4>()?;
        Some(format!("{}", u32::from_ne_bytes(*v)))
    } else {
        // C: sasprintf(&content, "%.*s", value_length, value);
        Some(String::from_utf8_lossy(&prop_reply.value).into_owned())
    }
}
