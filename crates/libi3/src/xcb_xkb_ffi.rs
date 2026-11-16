//! Hand-declared FFI for the libxcb-xkb requests of C src/main.c:840-887.
//!
//! C: src/main.c:841-887
//!     xcb_xkb_use_extension(conn, XCB_XKB_MAJOR_VERSION, XCB_XKB_MINOR_VERSION);
//!     xcb_xkb_select_events(conn, XCB_XKB_ID_USE_CORE_KBD, ...);
//!     xcb_xkb_per_client_flags(conn, XCB_XKB_ID_USE_CORE_KBD, mask, mask, 0, 0, 0);
//!
//! xcb_xkb_use_extension() is performed by xkb_x11_setup_xkb_extension() from
//! libxkbcommon-x11 (see xkbcommon_ffi.rs), which also reports the event base.
//! The other two requests are only available as xcb functions, so they are
//! declared here, like the cairo/pango declarations in cairo_ffi.rs.
//!
//! The declarations use the raw xcb_connection_t * because the x11rb xkb API
//! cannot express the C request: x11rb's SelectEventsAux derives affectWhich
//! from a fixed set of detail structs, and MAP_NOTIFY has no detail struct, so
//! the C mask (STATE_NOTIFY | MAP_NOTIFY | NEW_KEYBOARD_NOTIFY) is rejected by
//! its serializer. The C calls are therefore issued directly.

// The C names (xcb_void_cookie_t, XCB_XKB_EVENT_TYPE_*, …) are kept 1:1.
#![allow(non_camel_case_types, non_upper_case_globals)]

use std::ffi::c_void;

/// C: xcb/xcb.h xcb_void_cookie_t
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct xcb_void_cookie_t {
    pub sequence: u32,
}

/// C: xcb/xkb.h xcb_xkb_per_client_flags_cookie_t
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct xcb_xkb_per_client_flags_cookie_t {
    pub sequence: u32,
}

/// C: xcb/xkb.h xcb_xkb_per_client_flags_reply_t
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct xcb_xkb_per_client_flags_reply_t {
    pub response_type: u8,
    pub device_id: u8,
    pub sequence: u16,
    pub length: u32,
    pub supported: u32,
    pub value: u32,
    pub auto_ctrls: u32,
    pub auto_ctrls_values: u32,
    pub pad: [u8; 8],
}

/// C: xcb/xkb.h XCB_XKB_EVENT_TYPE_NEW_KEYBOARD_NOTIFY
pub const XCB_XKB_EVENT_TYPE_NEW_KEYBOARD_NOTIFY: u16 = 1 << 0;
/// C: xcb/xkb.h XCB_XKB_EVENT_TYPE_MAP_NOTIFY
pub const XCB_XKB_EVENT_TYPE_MAP_NOTIFY: u16 = 1 << 1;
/// C: xcb/xkb.h XCB_XKB_EVENT_TYPE_STATE_NOTIFY
pub const XCB_XKB_EVENT_TYPE_STATE_NOTIFY: u16 = 1 << 2;
/// C: xcb/xkb.h XCB_XKB_PER_CLIENT_FLAG_DETECTABLE_AUTO_REPEAT
pub const XCB_XKB_PER_CLIENT_FLAG_DETECTABLE_AUTO_REPEAT: u32 = 1 << 0;
/// C: xcb/xkb.h XCB_XKB_PER_CLIENT_FLAG_GRABS_USE_XKB_STATE
pub const XCB_XKB_PER_CLIENT_FLAG_GRABS_USE_XKB_STATE: u32 = 1 << 1;
/// C: xcb/xkb.h XCB_XKB_PER_CLIENT_FLAG_LOOKUP_STATE_WHEN_GRABBED
pub const XCB_XKB_PER_CLIENT_FLAG_LOOKUP_STATE_WHEN_GRABBED: u32 = 1 << 3;

extern "C" {
    fn xcb_xkb_select_events(
        conn: *mut c_void,
        device_spec: u16,
        affect_which: u16,
        clear: u16,
        select_all: u16,
        affect_map: u16,
        map: u16,
        details: *const c_void,
    ) -> xcb_void_cookie_t;
    fn xcb_xkb_per_client_flags(
        conn: *mut c_void,
        device_spec: u16,
        change: u32,
        value: u32,
        ctrls_to_change: u32,
        auto_ctrls: u32,
        auto_ctrls_values: u32,
    ) -> xcb_xkb_per_client_flags_cookie_t;
    fn xcb_xkb_per_client_flags_reply(
        conn: *mut c_void,
        cookie: xcb_xkb_per_client_flags_cookie_t,
        error: *mut *mut c_void,
    ) -> *mut xcb_xkb_per_client_flags_reply_t;
    /* C: libxcb; /usr/include/xcb/xcb.h */
    fn xcb_request_check(conn: *mut c_void, cookie: xcb_void_cookie_t) -> *mut c_void;
}

/// C: xcb_xkb_select_events(conn, XCB_XKB_ID_USE_CORE_KBD, mask, 0, mask,
///                          0xff, 0xff, NULL)
///
/// Returns the X error code of the request, or None when it succeeded.
pub fn select_events(
    conn: *mut c_void,
    device_spec: u16,
    affect_which: u16,
    clear: u16,
    select_all: u16,
    affect_map: u16,
    map: u16,
) -> Option<u8> {
    let cookie = unsafe {
        xcb_xkb_select_events(
            conn,
            device_spec,
            affect_which,
            clear,
            select_all,
            affect_map,
            map,
            std::ptr::null(),
        )
    };
    let error = unsafe { xcb_request_check(conn, cookie) };
    if error.is_null() {
        None
    } else {
        // C: xcb_generic_error_t->error_code
        Some(unsafe { *(error as *const u8) })
    }
}

/// C: xcb_xkb_per_client_flags(...) + xcb_xkb_per_client_flags_reply(...)
///
/// Returns (supported, value) — C only inspects `value` (PCF_REPLY_ERROR).
pub fn per_client_flags(
    conn: *mut c_void,
    device_spec: u16,
    change: u32,
    value: u32,
) -> Option<(u32, u32)> {
    let cookie = unsafe { xcb_xkb_per_client_flags(conn, device_spec, change, value, 0, 0, 0) };
    let mut error: *mut c_void = std::ptr::null_mut();
    let reply = unsafe { xcb_xkb_per_client_flags_reply(conn, cookie, &mut error) };
    if reply.is_null() {
        return None;
    }
    // SAFETY: the reply is owned by libxcb for this call and valid here.
    let reply = unsafe { *reply };
    Some((reply.supported, reply.value))
}
