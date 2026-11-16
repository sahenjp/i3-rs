//! C: libi3/fake_configure_notify.c

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConfigureNotifyEvent, EventMask, Window};

use crate::{xcb_rectangle_t, xcb_window_t};

/// C: xcb/xproto.h XCB_CONFIGURE_NOTIFY
pub const XCB_CONFIGURE_NOTIFY: u8 = 22;
/// C: xcb/xproto.h XCB_EVENT_MASK_STRUCTURE_NOTIFY = 131072 (1 << 17)
pub const XCB_EVENT_MASK_STRUCTURE_NOTIFY: u32 = 1 << 17;
/// C: XCB_NONE
pub const XCB_NONE: u32 = 0;

/// C: libi3/fake_configure_notify.c:25-39 — the xcb_configure_notify_event_t
/// the C code fills in.
///
/// C scalloc()s 32 bytes (every X11 event is 32 bytes, and XCB copies all of
/// them) and leaves the bytes after the 28-byte struct zero; x11rb's
/// ConfigureNotifyEvent serializes to the same 32 bytes.
pub fn configure_notify_event(
    window: xcb_window_t,
    r: xcb_rectangle_t,
    border_width: i32,
) -> ConfigureNotifyEvent {
    ConfigureNotifyEvent {
        response_type: XCB_CONFIGURE_NOTIFY,
        sequence: 0,
        /* C: 28-29 — generated_event->event = window; ->window = window */
        event: window,
        window,
        /* C: 33-35 */
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
        /* C: 37 — border_width is an int in the C signature, the field is a
         * CARD16 */
        border_width: border_width as u16,
        /* C: 38-39 — above_sibling = XCB_NONE, override_redirect = false */
        above_sibling: XCB_NONE,
        override_redirect: false,
    }
}

/// C: the connection half of fake_configure_notify(): xcb_send_event plus
/// xcb_flush.
pub trait SendEventConn {
    /// C: 41 xcb_send_event(conn, false, window,
    /// XCB_EVENT_MASK_STRUCTURE_NOTIFY, (char *)generated_event)
    fn send_event(
        &mut self,
        propagate: bool,
        destination: Window,
        event_mask: u32,
        event: ConfigureNotifyEvent,
    );
    /// C: 42 xcb_flush(conn)
    fn flush(&mut self);
}

/// C: libi3/fake_configure_notify.c:21 fake_configure_notify — generates a
/// configure_notify event and sends it to the given window. Applications need
/// this to think they've configured themselves correctly. The truth is,
/// however, that we will manage them.
pub fn fake_configure_notify(
    conn: &mut dyn SendEventConn,
    r: xcb_rectangle_t,
    window: xcb_window_t,
    border_width: i32,
) {
    let event = configure_notify_event(window, r, border_width);
    /* C: 41 */
    conn.send_event(false, window, XCB_EVENT_MASK_STRUCTURE_NOTIFY, event);
    /* C: 42 */
    conn.flush();
}

/// C: the two requests over the real x11rb connection (taken by shared
/// reference: every x11rb request only needs one).
impl<C> SendEventConn for &C
where
    C: Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
{
    fn send_event(
        &mut self,
        propagate: bool,
        destination: Window,
        event_mask: u32,
        event: ConfigureNotifyEvent,
    ) {
        let _ = x11rb::protocol::xproto::ConnectionExt::send_event(
            *self,
            propagate,
            destination,
            EventMask::from(event_mask),
            /* C passes the 32 bytes: x11rb converts the event the same way
             * (impl From<ConfigureNotifyEvent> for [u8; 32]). */
            event,
        );
    }

    fn flush(&mut self) {
        let _ = x11rb::connection::Connection::flush(*self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i16, y: i16, width: u16, height: u16) -> xcb_rectangle_t {
        xcb_rectangle_t {
            x,
            y,
            width,
            height,
        }
    }

    /// A conn that records the request instead of sending it.
    #[derive(Default)]
    struct RecordingSendConn {
        log: Vec<String>,
        sent: Option<(bool, Window, u32, ConfigureNotifyEvent)>,
    }

    impl SendEventConn for RecordingSendConn {
        fn send_event(
            &mut self,
            propagate: bool,
            destination: Window,
            event_mask: u32,
            event: ConfigureNotifyEvent,
        ) {
            self.log.push(format!(
                "send_event(win={:#x}, mask={:#x}, propagate={})",
                destination, event_mask, propagate
            ));
            self.sent = Some((propagate, destination, event_mask, event));
        }

        fn flush(&mut self) {
            self.log.push("flush".to_string());
        }
    }

    /// C: libi3/fake_configure_notify.c:26-39 — every field the C code sets.
    #[test]
    fn the_event_matches_the_c_struct() {
        let event = configure_notify_event(0x400000, rect(10, 20, 300, 200), 2);
        assert_eq!(event.response_type, XCB_CONFIGURE_NOTIFY);
        assert_eq!(event.response_type, 22);
        assert_eq!(event.sequence, 0);
        // C: generated_event->event = generated_event->window = window
        assert_eq!(event.event, 0x400000);
        assert_eq!(event.window, 0x400000);
        assert_eq!((event.x, event.y), (10, 20));
        assert_eq!((event.width, event.height), (300, 200));
        assert_eq!(event.border_width, 2);
        // C: above_sibling = XCB_NONE, override_redirect = false
        assert_eq!(event.above_sibling, XCB_NONE);
        assert!(!event.override_redirect);
    }

    /// C: the 32 bytes xcb_send_event copies: the event type first, then
    /// sequence, event, window, above_sibling, geometry, border width and the
    /// override-redirect flag; the last bytes stay zero (C scalloc()s 32).
    #[test]
    fn the_wire_format_is_32_bytes() {
        let bytes: [u8; 32] = configure_notify_event(0x400001, rect(1, 2, 3, 4), 5).into();
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[0], 22); // response_type
        assert_eq!(&bytes[1..2], &[0]); // pad
        assert_eq!(&bytes[2..4], &[0, 0]); // sequence
        assert_eq!(&bytes[4..8], &0x0040_0001u32.to_le_bytes()); // event
        assert_eq!(&bytes[8..12], &0x0040_0001u32.to_le_bytes()); // window
        assert_eq!(&bytes[12..16], &[0, 0, 0, 0]); // above_sibling = XCB_NONE
        assert_eq!(&bytes[16..18], &1i16.to_le_bytes()); // x
        assert_eq!(&bytes[18..20], &2i16.to_le_bytes()); // y
        assert_eq!(&bytes[20..22], &3u16.to_le_bytes()); // width
        assert_eq!(&bytes[22..24], &4u16.to_le_bytes()); // height
        assert_eq!(&bytes[24..26], &5u16.to_le_bytes()); // border_width
        assert_eq!(bytes[26], 0); // override_redirect
        assert_eq!(&bytes[27..32], &[0, 0, 0, 0, 0]); // padding
    }

    /// C: libi3/fake_configure_notify.c:41-42 — send_event with propagate
    /// false, the window as destination and the structure-notify mask, then
    /// flush.
    #[test]
    fn fake_configure_notify_sends_then_flushes() {
        let mut conn = RecordingSendConn::default();
        fake_configure_notify(&mut conn, rect(5, 6, 100, 50), 0x200003, 1);
        assert_eq!(
            conn.log,
            vec![
                "send_event(win=0x200003, mask=0x20000, propagate=false)",
                "flush"
            ]
        );
        let (propagate, destination, mask, event) = conn.sent.expect("no event");
        assert!(!propagate);
        assert_eq!(destination, 0x200003);
        assert_eq!(mask, XCB_EVENT_MASK_STRUCTURE_NOTIFY);
        assert_eq!(event.event, 0x200003);
        assert_eq!(
            (event.x, event.y, event.width, event.height),
            (5, 6, 100, 50)
        );
        assert_eq!(event.border_width, 1);
    }

    /// Real X (headless Xvfb): the event a client receives is the synthetic
    /// ConfigureNotify with the geometry we passed in.
    #[test]
    fn real_x_client_receives_the_synthetic_event() {
        use x11rb::protocol::Event;
        let Some(xvfb) = crate::test_util::start_xvfb() else {
            eprintln!("skipping: no Xvfb available");
            return;
        };
        let (conn, screen_num) = match x11rb::connect(Some(xvfb.display())) {
            Ok(conn) => conn,
            Err(err) => panic!("could not connect to {}: {err}", xvfb.display()),
        };
        let screen = conn.setup().roots[screen_num].clone();
        let window = conn.generate_id().expect("generate_id failed");
        x11rb::protocol::xproto::ConnectionExt::create_window(
            &conn,
            x11rb::COPY_FROM_PARENT as u8,
            window,
            screen.root,
            0,
            0,
            100,
            100,
            0,
            x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &x11rb::protocol::xproto::CreateWindowAux::new()
                .event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        /* C: the round trip that makes the window exist before the event is
         * sent (i3 issues plenty of requests before fake_configure_notify). */
        .expect("create_window failed")
        .check()
        .expect("create_window failed");
        let _ = x11rb::connection::Connection::flush(&conn);

        let mut conn_ref = &conn;
        fake_configure_notify(&mut conn_ref, rect(40, 30, 200, 150), window, 3);

        // The window's own CreateNotify comes first; the synthetic event is
        // what we are looking for. poll_for_event() does not block, so wait
        // for the round trip with a deadline.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut received = None;
        while std::time::Instant::now() < deadline {
            match conn.poll_for_event() {
                Ok(Some(Event::ConfigureNotify(event))) => {
                    received = Some(event);
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(err) => panic!("poll_for_event failed: {err}"),
            }
        }
        let event = received.expect("no ConfigureNotify received");
        assert_eq!(event.window, window);
        assert_eq!(event.event, window);
        assert_eq!((event.x, event.y), (40, 30));
        assert_eq!((event.width, event.height), (200, 150));
        assert_eq!(event.border_width, 3);
        assert_eq!(event.above_sibling, XCB_NONE);
        assert!(!event.override_redirect);
    }
}
