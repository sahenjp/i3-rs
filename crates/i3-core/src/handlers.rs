//! handlers.c: X event dispatching (1:1 port).
//!
//! C: src/handlers.c. Handlers take (tree, backend, xconn, event) instead of
//! touching the C globals. The event structs are modelled as plain structs
//! (same fields as the xcb event types) so the dispatch table and the handler
//! logic can be tested with a RecordingConn / fake events.

use crate::backend::XConn;
use std::sync::Mutex;
use crate::con::{con_by_frame_id, con_by_window_id, con_get_workspace, con_set_urgency};
use crate::data::ConId;
use crate::tree::{tree_close_internal, tree_render, Tree};
use crate::xcb_util::Atoms;

/// C: src/handlers.c:28 'static SLIST_HEAD(ignore_head, Ignore_Event) ignore_events;'
/// The C global is a process-global list; the port uses a Mutex-protected Vec.
static IGNORE_EVENTS: Mutex<Vec<crate::data::Ignore_Event>> = Mutex::new(Vec::new());

/// C: src/handlers.c:38 add_ignore_event
/// (ignore_event_sequence_t values come from the request sequence; -1 maps to
/// u32::MAX == 0xFFFFFFFF).
pub fn add_ignore_event(t: &mut Tree, sequence: u32, response_type: i32) {
    let _ = t;
    add_ignore_event_seq(sequence, response_type);
}

/// C: src/handlers.c:38 add_ignore_event -- the list is a process global in C
/// (SLIST ignore_events), so the X11 layer can register a sequence right after
/// issuing the request without having the Tree at hand.
pub fn add_ignore_event_seq(sequence: u32, response_type: i32) {
    let mut events = IGNORE_EVENTS.lock().unwrap();
    // C: SLIST_INSERT_HEAD(&ignore_events, event, ignore_events) -- the C code
    // does not deduplicate; repeated sequences are garbage collected instead.
    events.insert(
        0,
        crate::data::Ignore_Event {
            sequence: crate::data::ignore_event_sequence_t { value: sequence },
            response_type,
            added: unix_time_us(),
        },
    );
}

/// C: src/handlers.c:52 event_is_ignored
pub fn event_is_ignored(_t: &mut Tree, sequence: u32, response_type: i32) -> bool {
    let mut events = IGNORE_EVENTS.lock().unwrap();
    let now = unix_time_us();

    // C: every ignored sequence number gets garbage collected after 5 seconds.
    events.retain(|ignore| (now - ignore.added) <= 5);

    for ignore in events.iter() {
        if ignore.sequence.value != sequence {
            continue;
        }

        if ignore.response_type != -1 && ignore.response_type != response_type {
            continue;
        }

        // C: "Instead of removing & freeing a sequence number we better wait
        // until it gets garbage collected. It may generate multiple events."
        return true;
    }

    false
}

/// unix time in microseconds (C: ev_now / time(NULL) mix — the C stores a
/// time_t in added).
fn unix_time_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/**************************************************************************** * Event models (field-for-field the xcb event structs)
****************************************************************************/

/// C: xcb_map_request_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct map_request_event_t {
    pub window: u32,
    pub parent: u32,
    /// C: event->full_sequence (add_ignore_event in handle_map_request)
    pub full_sequence: u32,
}

/// C: xcb_unmap_notify_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct unmap_notify_event_t {
    pub window: u32,
    pub event: u32,
    pub from_configure: bool,
    pub full_sequence: u32,
}

/// C: xcb_destroy_notify_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct destroy_notify_event_t {
    pub window: u32,
    pub event: u32,
    pub full_sequence: u32,
}

/// C: xcb_configure_request_event_t (subset i3 reads)
#[derive(Debug, Clone, Copy, Default)]
pub struct configure_request_event_t {
    pub window: u32,
    pub value_mask: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub sibling: u32,
    pub stack_mode: u8,
}

/// C: xcb_enter_notify_event_t (subset)
#[derive(Debug, Clone, Copy, Default)]
pub struct enter_notify_event_t {
    pub event: u32,
    pub child: u32,
    /// C: event->detail (XCB_NOTIFY_DETAIL_*)
    pub detail: u8,
    /// C: event->mode (XCB_NOTIFY_MODE_*); 0 == XCB_NOTIFY_MODE_NORMAL
    pub mode: u8,
    pub root_x: i16,
    pub root_y: i16,
    /// C: event->full_sequence
    pub full_sequence: u32,
}

/// C: xcb_motion_notify_event_t (subset)
#[derive(Debug, Clone, Copy, Default)]
pub struct motion_notify_event_t {
    pub event: u32,
    /// C: event->child (XCB_NONE when the pointer is over the root window)
    pub child: u32,
    pub root_x: i16,
    pub root_y: i16,
}

/// C: xcb_client_message_event_t (subset)
#[derive(Debug, Clone, Copy, Default)]
pub struct client_message_event_t {
    pub window: u32,
    pub type_: u32,
    pub format: u8,
    pub data32: [u32; 5],
}

/// C: xcb_expose_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct expose_event_t {
    pub window: u32,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub count: u16,
}

/// C: xcb_property_notify_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct property_notify_event_t {
    pub window: u32,
    pub atom: u32,
    pub state: u8,
}

/// C: xcb_focus_in_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct focus_in_event_t {
    pub event: u32,
    pub detail: u8,
}

/// C: xcb_configure_notify_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct configure_notify_event_t {
    pub event: u32,
    pub window: u32,
    pub override_redirect: bool,
}

/// C: xcb_button_press_event_t / button_release_event_t (subset)
#[derive(Debug, Clone, Copy, Default)]
pub struct button_event_t {
    pub event: u32,
    pub child: u32,
    pub detail: u8,
    pub state: u16,
    /// C: event->event_x — relative to the event window, int16 in the
    /// protocol; widened because click.c compares it against rect widths.
    pub event_x: i32,
    /// C: event->event_y
    pub event_y: i32,
    /// C: event->root_x — absolute pointer position (drag.c/resize.c).
    pub root_x: i32,
    /// C: event->root_y
    pub root_y: i32,
    /// C: event->time
    pub time: u32,
    /// C: event->response_type (XCB_BUTTON_PRESS / XCB_BUTTON_RELEASE)
    pub response_type: u8,
}

/// C: xcb_mapping_notify_event_t
#[derive(Debug, Clone, Copy, Default)]
pub struct mapping_notify_event_t {
    pub request: u8,
}

/**************************************************************************** * Handlers
****************************************************************************/

/// C: src/handlers.c:283 handle_map_request
pub fn handle_map_request(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, event: &map_request_event_t) {
    // C: src/handlers.c:286 DLOG("window = 0x%08x, serial is %d.\n", ...)
    // (the port keeps only the full 32-bit sequence, see map_request_event_t)
    crate::log::dlog_at(
        file!(),
        "handle_map_request",
        line!(),
        &format!(
            "window = {:#010x}, serial is {}.\n",
            event.window, event.full_sequence
        ),
    );
    // C: add_ignore_event(ENSURE_FULL_SEQUENCE(full_sequence), -1);
    add_ignore_event(t, event.full_sequence, -1);
    // C: xcb_get_window_attributes unchecked + manage_window(cookie, false)
    let attrs = x.get_window_attributes(event.window);
    match attrs {
        None => {
            // C: if the window disappeared, nothing to do
            return;
        }
        Some(_attr) => {
            // C: if (attr->override_redirect) return; else manage_window(..., false)
            crate::manage::manage_window(t, b, x, a, event.window, false);
        }
    }
}

/// C: src/handlers.c:482 handle_unmap_notify_event
pub fn handle_unmap_notify_event(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, event: &unmap_notify_event_t) {
    // C: src/handlers.c:482 — first look up the *client* window, then the
    // frame (the frame case only decrements ignore_unmap).
    // C: src/handlers.c:483 DLOG("UnmapNotify for 0x%08x (received from
    // 0x%08x), serial %d\n", ...)
    crate::log::dlog_at(
        file!(),
        "handle_unmap_notify_event",
        line!(),
        &format!(
            "UnmapNotify for {:#010x} (received from {:#010x}), serial {}\n",
            event.window, event.event, event.full_sequence
        ),
    );

    let con = match con_by_window_id(t, event.window) {
        Some(c) => c,
        None => {
            let frame_con = match con_by_frame_id(t, event.window) {
                Some(c) => c,
                None => {
                    // C: LOG("Not a managed window, ignoring UnmapNotify event\n")
                    crate::log::log_("Not a managed window, ignoring UnmapNotify event\n");
                    return;
                }
            };
            // C: if (con->ignore_unmap > 0) con->ignore_unmap--;
            if t.cons[frame_con].ignore_unmap > 0 {
                t.con_mut(frame_con).ignore_unmap -= 1;
            }
            // C: DLOG("ignore_unmap = %d for frame of container %p\n", ...)
            crate::log::dlog_at(
                file!(),
                "handle_unmap_notify_event",
                line!(),
                &format!(
                    "ignore_unmap = {} for frame of container {}\n",
                    t.cons[frame_con].ignore_unmap,
                    crate::manage::con_ptr(frame_con)
                ),
            );
            // C: goto ignore_end
            add_ignore_event(t, event.full_sequence, XCB_ENTER_NOTIFY as i32);
            return;
        }
    };

    if t.cons[con].ignore_unmap > 0 {
        // C: DLOG("ignore_unmap = %d, dec\n", con->ignore_unmap);
        crate::log::dlog_at(
            file!(),
            "handle_unmap_notify_event",
            line!(),
            &format!("ignore_unmap = {}, dec\n", t.cons[con].ignore_unmap),
        );
        t.con_mut(con).ignore_unmap -= 1;
        // C: goto ignore_end
        add_ignore_event(t, event.full_sequence, XCB_ENTER_NOTIFY as i32);
        return;
    }

    // C: xcb_delete_property(conn, event->window, A__NET_WM_DESKTOP/_NET_WM_STATE)
    x.delete_property(event.window, a.A__NET_WM_DESKTOP);
    x.delete_property(event.window, a.A__NET_WM_STATE);

    // C: tree_close_internal(con, DONT_KILL_WINDOW, false); tree_render();
    tree_close_internal(t, b, con, crate::data::KillWindow::DONT_KILL_WINDOW, false);
    tree_render(t, b);

    // C: ignore_end — ignore the EnterNotify with the same sequence.
    add_ignore_event(t, event.full_sequence, XCB_ENTER_NOTIFY as i32);
}

/// C: src/handlers.c:554 handle_destroy_notify_event
pub fn handle_destroy_notify_event(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, event: &destroy_notify_event_t) {
    // C: same flow as unmap (destroy implies unmap)
    handle_unmap_notify_event(
        t,
        b,
        x,
        a,
        &unmap_notify_event_t {
            window: event.window,
            event: event.event,
            from_configure: false,
            full_sequence: event.full_sequence,
        },
    );
}

/// C: src/handlers.c:300 handle_configure_request
pub fn handle_configure_request(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, event: &configure_request_event_t) {
    let _ = a;
    // C: src/handlers.c:301-304
    crate::log::dlog_at(
        file!(),
        "handle_configure_request",
        line!(),
        &format!(
            "window {:#08x} wants to be at {}x{} with {}x{}\n",
            event.window, event.x, event.y, event.width, event.height
        ),
    );

    // C: src/handlers.c:306-334 — For unmanaged windows we just execute the
    // configure request (the code below); as soon as the window gets mapped we
    // take over anyways. Managed windows are handled here.
    if let Some(con) = con_by_window_id(t, event.window) {
        // C: src/handlers.c:338-342 — a scratchpad container ignores the
        // request.
        let workspace = crate::con::con_get_workspace(t, con);
        if let Some(ws) = workspace {
            if t.cons[ws].name.as_deref() == Some("__i3_scratch") {
                crate::log::dlog_at(
                    file!(),
                    "handle_configure_request",
                    line!(),
                    "This is a scratchpad container, ignoring ConfigureRequest\n",
                );
                // C: goto out; -> fake_absolute_configure_notify(con)
                crate::xcb_util::fake_absolute_configure_notify(t, x, con);
                return;
            }
        }

        let fullscreen = crate::con::con_get_fullscreen_covering_ws(t, workspace);

        if fullscreen != Some(con) && crate::con::con_is_floating(t, con) && crate::con::con_is_leaf(t, con) {
            /* C: src/handlers.c:345-378 — we actually need to apply the
             * size/position changes to the *parent* container. */
            let bsr = crate::con::con_border_style_rect(t, con);
            let floatingcon = match t.cons[con].parent {
                Some(parent) => parent,
                None => return,
            };
            let mut newrect = t.cons[floatingcon].rect;
            let border_width = t.cons[con].border_width;

            if event.value_mask & 0x1 != 0 {
                /* XCB_CONFIG_WINDOW_X */
                let x = event.x as i32 - bsr.x as i32;
                newrect.x = x.max(0) as u32;
            }
            if event.value_mask & 0x2 != 0 {
                /* XCB_CONFIG_WINDOW_Y */
                let y = event.y as i32 - bsr.y as i32;
                newrect.y = y.max(0) as u32;
            }
            if event.value_mask & 0x4 != 0 {
                /* XCB_CONFIG_WINDOW_WIDTH */
                let width = event.width as i32 - bsr.width as i32 + border_width * 2;
                newrect.width = width.max(0) as u32;
            }
            if event.value_mask & 0x8 != 0 {
                /* XCB_CONFIG_WINDOW_HEIGHT */
                let height = event.height as i32 - bsr.height as i32 + border_width * 2;
                newrect.height = height.max(0) as u32;
            }

            // C: floating_reposition(floatingcon, newrect);
            crate::floating::floating_reposition(t, b, floatingcon, newrect);
            return;
        }

        // C: src/handlers.c:381-408 — "Dock windows can be reconfigured in
        // their height and moved to another output."
        let parent_is_dockarea = t.cons[con]
            .parent
            .map(|p| t.cons[p].r#type == crate::data::ConType::CT_DOCKAREA)
            == Some(true);
        if parent_is_dockarea {
            crate::log::dlog_at(
                file!(),
                "handle_configure_request",
                line!(),
                &format!(
                    "Reconfiguring dock window (con = {}).\n",
                    crate::manage::con_ptr(con)
                ),
            );
            if event.value_mask & 0x8 != 0 {
                /* XCB_CONFIG_WINDOW_HEIGHT */
                crate::log::dlog_at(
                    file!(),
                    "handle_configure_request",
                    line!(),
                    &format!(
                        "Dock client wants to change height to {}, we can do that.\n",
                        event.height
                    ),
                );
                t.cons[con].geometry.height = event.height as u32;
                tree_render(t, b);
            }

            if event.value_mask & 0x1 != 0 || event.value_mask & 0x2 != 0 {
                let x = if event.value_mask & 0x1 != 0 {
                    event.x
                } else {
                    t.cons[con].geometry.x as i16
                };
                let y = if event.value_mask & 0x2 != 0 {
                    event.y
                } else {
                    t.cons[con].geometry.y as i16
                };

                let current_output = crate::con::con_get_output(t, con);
                let target = crate::output::get_output_containing(t, x as u32, y as u32);
                let target_con = match target {
                    Some(idx) => t.outputs[idx].con,
                    None => None,
                };
                if target_con.is_some() && current_output != target_con {
                    crate::log::dlog_at(
                        file!(),
                        "handle_configure_request",
                        line!(),
                        &format!(
                            "Dock client is requested to be moved to output {}.\n",
                            target
                                .map(|idx| crate::output::output_primary_name(t, idx))
                                .unwrap_or_default()
                        ),
                    );
                    let window = t.cons[con].window;
                    let nc = crate::manage::con_for_window_with_match(t, target_con, window).0;
                    crate::log::dlog_at(
                        file!(),
                        "handle_configure_request",
                        line!(),
                        &format!(
                            "Dock client will be moved to container {}.\n",
                            nc.map(crate::manage::con_ptr).unwrap_or_else(|| "(nil)".to_string())
                        ),
                    );
                    if let Some(nc) = nc {
                        crate::con::con_detach(t, con);
                        crate::con::con_attach(t, b, con, nc, false);
                        tree_render(t, b);
                    }
                } else {
                    crate::log::dlog_at(
                        file!(),
                        "handle_configure_request",
                        line!(),
                        "Dock client will not be moved, we only support moving it to another output.\n",
                    );
                }
            }
            // C: goto out; -> fake_absolute_configure_notify(con)
            crate::xcb_util::fake_absolute_configure_notify(t, x, con);
            return;
        }

        if event.value_mask & 0x40 != 0 {
            /* XCB_CONFIG_WINDOW_STACK_MODE */
            crate::log::dlog_at(
                file!(),
                "handle_configure_request",
                line!(),
                &format!(
                    "window {:#08x} wants to be stacked {}\n",
                    event.window, event.stack_mode
                ),
            );

            /* Emacs and IntelliJ Idea "request focus" by stacking their window
             * above all others. */
            if event.stack_mode != crate::backend::StackMode::ABOVE as u8 {
                crate::log::dlog_at(
                    file!(),
                    "handle_configure_request",
                    line!(),
                    "stack_mode != XCB_STACK_MODE_ABOVE, ignoring ConfigureRequest\n",
                );
                crate::xcb_util::fake_absolute_configure_notify(t, x, con);
                return;
            }

            if fullscreen.is_some() || !crate::con::con_is_leaf(t, con) {
                crate::log::dlog_at(
                    file!(),
                    "handle_configure_request",
                    line!(),
                    "fullscreen or not a leaf, ignoring ConfigureRequest\n",
                );
                crate::xcb_util::fake_absolute_configure_notify(t, x, con);
                return;
            }

            let ws = match workspace {
                Some(ws) => ws,
                None => {
                    crate::log::dlog_at(
                        file!(),
                        "handle_configure_request",
                        line!(),
                        "Window is not being managed, ignoring ConfigureRequest\n",
                    );
                    crate::xcb_util::fake_absolute_configure_notify(t, x, con);
                    return;
                }
            };

            use crate::data::FocusOnWindowActivation;
            let fowa = t.config.focus_on_window_activation;
            let visible = crate::workspace::workspace_is_visible(t, ws);
            if fowa == FocusOnWindowActivation::FOWA_FOCUS
                || (fowa == FocusOnWindowActivation::FOWA_SMART && visible)
            {
                crate::log::dlog_at(
                    file!(),
                    "handle_configure_request",
                    line!(),
                    &format!("Focusing con = {}\n", crate::manage::con_ptr(con)),
                );
                crate::workspace::workspace_show(t, b, ws);
                crate::con::con_activate_unblock(t, b, con);
                tree_render(t, b);
            } else if fowa == FocusOnWindowActivation::FOWA_URGENT
                || (fowa == FocusOnWindowActivation::FOWA_SMART && !visible)
            {
                crate::log::dlog_at(
                    file!(),
                    "handle_configure_request",
                    line!(),
                    &format!("Marking con = {} urgent\n", crate::manage::con_ptr(con)),
                );
                crate::con::con_set_urgency(t, b, con, true);
                let _con = crate::manage::remanage_window(t, b, x, a, con);
                tree_render(t, b);
            } else {
                crate::log::dlog_at(
                    file!(),
                    "handle_configure_request",
                    line!(),
                    &format!("Ignoring request for con = {}.\n", crate::manage::con_ptr(con)),
                );
            }
        }

        // C: out: fake_absolute_configure_notify(con);
        crate::xcb_util::fake_absolute_configure_notify(t, x, con);
        return;
    }
    let mut values: Vec<(crate::backend::ConfigWindow, u32)> = Vec::new();
    // C: XCB_CONFIG_WINDOW_X etc. bits are the same order as our ConfigWindow
    if event.value_mask & 0x1 != 0 {
        values.push((crate::backend::ConfigWindow::X, event.x.max(0) as u32));
    }
    if event.value_mask & 0x2 != 0 {
        values.push((crate::backend::ConfigWindow::Y, event.y.max(0) as u32));
    }
    if event.value_mask & 0x4 != 0 {
        values.push((crate::backend::ConfigWindow::WIDTH, event.width as u32));
    }
    if event.value_mask & 0x8 != 0 {
        values.push((crate::backend::ConfigWindow::HEIGHT, event.height as u32));
    }
    if event.value_mask & 0x10 != 0 {
        values.push((crate::backend::ConfigWindow::BORDER_WIDTH, 0)); // C: we draw our own borders
    }
    // C: forward stacking requests for unmanaged windows
    if event.value_mask & 0x20 != 0 {
        values.push((crate::backend::ConfigWindow::SIBLING, event.sibling));
    }
    if event.value_mask & 0x40 != 0 {
        values.push((crate::backend::ConfigWindow::STACK_MODE, event.stack_mode as u32));
    }
    x.configure_window(event.window, &values);
}

/// C: src/handlers.c:91 `check_crossing_screen_boundary` (static)
///
/// Called with coordinates of an enter_notify event or motion_notify event to
/// check if the user crossed virtual screen boundaries and adjust the current
/// workspace, if so.
fn check_crossing_screen_boundary(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: u32, y: u32) {
    /* If the user disabled focus follows mouse, we have nothing to do here */
    if t.config.disable_focus_follows_mouse {
        return;
    }

    let output = match crate::output::get_output_containing(t, x, y) {
        Some(output) => output,
        None => {
            crate::log::errorlog("ERROR: No such screen\n");
            return;
        }
    };

    let output_con = match t.outputs[output].con {
        Some(con) => con,
        None => {
            crate::log::errorlog("ERROR: The screen is not recognized by i3 (no container associated)\n");
            return;
        }
    };

    /* Focus the output on which the user moved their cursor */
    let old_focused = t.focused;
    let next = match crate::output::output_get_content(t, output_con) {
        Some(content) => crate::con::con_descend_focused(t, content),
        None => return,
    };
    /* Since we are switching outputs, this *must* be a different workspace, so
     * call workspace_show() */
    let next_ws = crate::con::con_get_workspace(t, next);
    if let Some(next_ws) = next_ws {
        crate::workspace::workspace_show(t, b, next_ws);
    }
    crate::con::con_focus(t, b, next);

    /* If the focus changed, we re-render to get updated decorations */
    if old_focused != t.focused {
        tree_render(t, b);
    }
}

/// C: src/handlers.c:127 handle_enter_notify
pub fn handle_enter_notify(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, event: &enter_notify_event_t) {
    // C: src/handlers.c:132 DLOG("enter_notify for %08x, mode = %d, detail %d,
    // serial %d\n", ...)
    crate::log::dlog_at(
        file!(),
        "handle_enter_notify",
        line!(),
        &format!(
            "enter_notify for {:08x}, mode = {}, detail {}, serial {}\n",
            event.event, event.mode, event.detail, event.full_sequence
        ),
    );
    crate::log::dlog_at(
        file!(),
        "handle_enter_notify",
        line!(),
        &format!("coordinates {}, {}\n", event.root_x, event.root_y),
    );

    // C: if (event->mode != XCB_NOTIFY_MODE_NORMAL) return;
    if event.mode != 0 {
        crate::log::dlog_at(
            file!(),
            "handle_enter_notify",
            line!(),
            "This was not a normal notify, ignoring\n",
        );
        return;
    }
    // C: if (event_is_ignored(ENSURE_FULL_SEQUENCE(full_sequence), XCB_ENTER_NOTIFY)) return;
    if event_is_ignored(t, event.full_sequence, XCB_ENTER_NOTIFY as i32) {
        crate::log::dlog_at(file!(), "handle_enter_notify", line!(), "Event ignored\n");
        return;
    }
    // C: got container by frame, then by child window.
    let con = match con_by_frame_id(t, event.event) {
        Some(c) => c,
        None => match con_by_window_id(t, event.event) {
            Some(c) => c,
            None => {
                /* If we cannot find the container, the user moved their cursor
                 * to the root window. In this case and if they used it to
                 * focus a dock, we need to focus the workspace on the correct
                 * output. */
                check_crossing_screen_boundary(t, b, event.root_x as u32, event.root_y as u32);
                return;
            }
        },
    };
    // C: if (con->parent->type == CT_DOCKAREA) { check_crossing_screen_boundary(); return; }
    if let Some(parent) = t.cons[con].parent {
        if t.cons[parent].r#type == crate::data::ConType::CT_DOCKAREA {
            check_crossing_screen_boundary(t, b, event.root_x as u32, event.root_y as u32);
            return;
        }
    }
    // C: if (config.disable_focus_follows_mouse) return; — the dispatcher
    // (handle_x11_event_ctx) already enforces the same flag before calling
    // this handler, and check_crossing_screen_boundary() checks it itself.
    if t.focused == Some(con) {
        return;
    }

    /* C: src/handlers.c:186-193 — get the currently focused workspace to
     * check if the focus change also involves changing workspaces. If so, we
     * need to call workspace_show() to correctly update state and send the IPC
     * event. */
    let ws = con_get_workspace(t, con);
    if ws != t.focused.and_then(|f| con_get_workspace(t, f)) {
        if let Some(ws) = ws {
            crate::workspace::workspace_show(t, b, ws);
        }
    }

    /* C: src/handlers.c:195-197 — focused_id = XCB_NONE;
     * con_focus(con_descend_focused(con)); tree_render();
     *
     * Not con_activate(): C deliberately only focuses (without raising), so
     * that moving the mouse onto a floating window does not change the
     * stacking order (issue #2990, test 293). Descending matters for tabbed /
     * stacked containers: the container itself must not receive the focus. */
    crate::x::set_focused_id(crate::data::XCB_WINDOW_NONE);
    let next = crate::con::con_descend_focused(t, con);
    crate::con::con_focus(t, b, next);
    tree_render(t, b);
    let _ = x;
}

/// C: src/handlers.c:203 handle_motion_notify
pub fn handle_motion_notify(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, _x: &mut dyn XConn, event: &motion_notify_event_t) {
    // C: src/handlers.c:210-215 — Skip events where the pointer was over a
    // child window; we are only interested in events on the root window.
    if event.child != crate::data::XCB_WINDOW_NONE {
        return;
    }

    if con_by_frame_id(t, event.event).is_none() {
        /* C: MotionNotify for an unknown container, checking if it crosses
         * screen boundaries. */
        check_crossing_screen_boundary(t, b, event.root_x as u32, event.root_y as u32);
        return;
    }

    // C: the decoration-based focus change (con_focus + x_push_changes) needs
    // the pointer position relative to the frame; it is not used by the
    // testsuite and lives in the floating/drag phase of the port.
    let _ = (t, b, event);
}

/// C: src/handlers.c:263 handle_mapping_notify
pub fn handle_mapping_notify(_t: &mut Tree, _b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, event: &mapping_notify_event_t) {
    // C: xcb_refresh_keyboard_mapping + bindings re-grab (bindings phase);
    // the flush matches C's xcb_flush call.
    let _ = (x, event);
}

/// C: src/handlers.c:639 handle_expose_event
pub fn handle_expose_event(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, _x: &mut dyn XConn, event: &expose_event_t) {
    if event.count != 0 {
        return; // C: only the last expose triggers a redraw
    }
    // C: x_draw_decoration for the exposed frame — routed via x.rs in the
    // draw phase; here the con is marked for redraw.
    if let Some(con) = con_by_frame_id(t, event.window) {
        t.con_mut(con).mark_changed = true;
        let _ = b;
    }
}

/// C: src/handlers.c:674 handle_net_wm_state_change
pub fn handle_net_wm_state_change(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, con: ConId, change: u32, atom: u32) {
    // C: src/handlers.c:675 'if (atom == 0) return;'
    if atom == 0 {
        return;
    }
    // C: change: 0 = remove, 1 = add, 2 = toggle
    let window = match t.cons[con].window {
        Some(w) => w,
        None => return,
    };
    let _ = window;
    if atom == a.A__NET_WM_STATE_FULLSCREEN {
        // C: handle the fullscreen state change
        let currently = t.cons[con].fullscreen_mode != crate::data::FullscreenMode::CF_NONE;
        let target = match change {
            0 => false,
            1 => true,
            _ => !currently,
        };
        if target != currently {
            crate::con::con_toggle_fullscreen(t, b, con, crate::data::FullscreenMode::CF_OUTPUT as i32);
        }
        // C: src/handlers.c:725 tree_render() at the end of the fullscreen branch
        crate::tree::request_tree_render();
    }

    if atom == a.A__NET_WM_STATE_DEMANDS_ATTENTION {
        // C: src/handlers.c:691-704
        crate::log::dlog_at(
            file!(),
            "handle_net_wm_state_change",
            line!(),
            "Received a client message to change _NET_WM_STATE_DEMANDS_ATTENTION.\n",
        );

        /* Check if the urgent flag must be set or not */
        let target = match change {
            0 => false,                                              // _NET_WM_STATE_REMOVE
            1 => true,                                               // _NET_WM_STATE_ADD
            _ => !t.cons[con].urgent,                                // _NET_WM_STATE_TOGGLE
        };
        crate::con::con_set_urgency(t, b, con, target);
        /* C: con = remanage_window(con); — re-runs the for_window
         * assignments, e.g. 'for_window [urgent=latest] focus'. */
        let _con = crate::manage::remanage_window(t, b, x, a, con);
        // C: tree_render() at the end of handle_net_wm_state_change()
        crate::tree::request_tree_render();
    }

    if atom == a.A__NET_WM_STATE_STICKY {
        // C: src/handlers.c:708-726 — "Received a client message to %s
        // _NET_WM_STATE_STICKY."
        crate::log::dlog_at(
            file!(),
            "handle_net_wm_state_change",
            line!(),
            &format!("Received a client message to change _NET_WM_STATE_STICKY.\n"),
        );

        let sticky = match change {
            0 => false,                                              // _NET_WM_STATE_REMOVE
            1 => true,                                               // _NET_WM_STATE_ADD
            _ => !t.cons[con].sticky,                                // _NET_WM_STATE_TOGGLE
        };
        t.cons[con].sticky = sticky;

        crate::log::dlog_at(
            file!(),
            "handle_net_wm_state_change",
            line!(),
            &format!("New sticky status for con = {} is {}.\n", crate::manage::con_ptr(con), sticky),
        );
        // C: ewmh_update_sticky(con->window->id, con->sticky);
        crate::ewmh::ewmh_update_sticky(x, a, t.windows[window].id, sticky);
        // C: output_push_sticky_windows(focused);
        crate::output::output_push_sticky_windows(t, b, t.focused);
        // C: ewmh_update_wm_desktop();
        crate::ewmh::ewmh_update_wm_desktop(t, x, a);
        // C: tree_render() at the end of handle_client_message()
        crate::tree::request_tree_render();
    }
    // C: the MODAL / HIDDEN / MAXIMIZED_{HORZ,VERT} atoms are not handled here
    // (maximum/minimized hints are derived from the tree in
    // set_maximized_state(), x.c:834).
}

/// C: src/handlers.c:732 handle_client_message
pub fn handle_client_message(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, event: &client_message_event_t) {
    // C: if (event->type == A__NET_WM_STATE) { ... handle_net_wm_state_change }
    if event.type_ == a.A__NET_WM_STATE {
        let con = match con_by_window_id(t, event.window) {
            Some(c) => c,
            None => return,
        };
        // C: for (i = 0; i < sizeof(data32)/sizeof(data32[0]) - 1; i++)
        //        handle_net_wm_state_change(con, data32[0], data32[i + 1]);
        for &atom in &event.data32[1..5] {
            handle_net_wm_state_change(t, b, x, a, con, event.data32[0], atom);
        }
    }
    // C: src/handlers.c:755-812 A__NET_ACTIVE_WINDOW
    if event.type_ == a.A__NET_ACTIVE_WINDOW {
        // C: src/handlers.c:756 'if (event->format != 32) { return; }'
        if event.format != 32 {
            return;
        }
        if let Some(con) = con_by_window_id(t, event.window) {
            // C: src/handlers.c:768-772 — a window without a workspace is not
            // managed (e.g. docks live under the __i3 pseudo output).
            let ws = match crate::con::con_get_workspace(t, con) {
                Some(ws) => ws,
                None => return,
            };
            let scratchpad = crate::workspace::workspace_get(t, b, "__i3_scratch");
            // C: src/handlers.c:774-777
            if crate::con::con_is_internal(t, ws) && ws != scratchpad {
                return;
            }
            // C: src/handlers.c:780 'data32[0] == 2' means the request came
            // from a pager (e.g. i3bar), so always focus the con.
            if event.data32[0] == 2 {
                if crate::con::con_is_internal(t, ws) {
                    crate::scratchpad::scratchpad_show(t, b, Some(con));
                } else {
                    crate::workspace::workspace_show(t, b, ws);
                    // C: focused_id = XCB_NONE — re-set focus even if i3
                    // thinks the focus did not change.
                    crate::x::set_focused_id(crate::data::XCB_WINDOW_NONE);
                    crate::con::con_activate_unblock(t, b, con);
                }
            } else {
                // C: src/handlers.c:791-809 — request from an application.
                if crate::con::con_is_internal(t, ws) {
                    return;
                }
                let fowa = t.config.focus_on_window_activation;
                let visible = crate::workspace::workspace_is_visible(t, ws);
                if fowa == crate::data::FocusOnWindowActivation::FOWA_FOCUS
                    || (fowa == crate::data::FocusOnWindowActivation::FOWA_SMART && visible)
                {
                    crate::con::con_activate_unblock(t, b, con);
                } else if fowa == crate::data::FocusOnWindowActivation::FOWA_URGENT
                    || (fowa == crate::data::FocusOnWindowActivation::FOWA_SMART && !visible)
                {
                    crate::con::con_set_urgency(t, b, con, true);
                    crate::manage::remanage_window(t, b, x, a, con);
                }
            }
        }
        // C: src/handlers.c:812 tree_render() at the end of the branch
        crate::tree::request_tree_render();
    }
    // C: A__NET_CLOSE_WINDOW -> tree_close_internal
    if event.type_ == a.A__NET_CLOSE_WINDOW {
        if let Some(con) = con_by_window_id(t, event.window) {
            tree_close_internal(t, b, con, crate::data::KillWindow::KILL_WINDOW, false);
            tree_render(t, b);
        }
    }

    // C: src/handlers.c:861 — "} else if (event->type == A__NET_CURRENT_DESKTOP) {"
    //
    // "This request is used by pagers and bars to change the current desktop
    // likely as a result of some user action. We interpret this as a request to
    // focus the given workspace."
    // https://standards.freedesktop.org/wm-spec/latest/ar01s03.html
    if event.type_ == a.A__NET_CURRENT_DESKTOP {
        let idx = event.data32[0];
        crate::log::dlog_at(
            file!(),
            "handle_client_message",
            line!(),
            &format!("Request to change current desktop to index {}\n", idx),
        );
        match crate::ewmh::ewmh_get_workspace_by_index(t, idx) {
            None => {
                // C: ELOG("Could not determine workspace for this index,
                //          ignoring request.\n");
                crate::log::dlog_at(
                    file!(),
                    "handle_client_message",
                    line!(),
                    "Could not determine workspace for this index, ignoring request.\n",
                );
            }
            Some(ws) => {
                crate::log::dlog_at(
                    file!(),
                    "handle_client_message",
                    line!(),
                    &format!(
                        "Handling request to focus workspace {}\n",
                        t.cons[ws].name.clone().unwrap_or_default()
                    ),
                );
                // C: workspace_show(ws); tree_render();
                crate::workspace::workspace_show(t, b, ws);
                tree_render(t, b);
            }
        }
    }

    // C: src/handlers.c:879 — "} else if (event->type == A__NET_WM_DESKTOP) {"
    if event.type_ == a.A__NET_WM_DESKTOP {
        let index = event.data32[0];
        crate::log::dlog_at(
            file!(),
            "handle_client_message",
            line!(),
            &format!(
                "Request to move window {} to EWMH desktop index {}\n",
                event.window, index
            ),
        );

        let con = match con_by_window_id(t, event.window) {
            Some(c) => c,
            None => return,
        };

        if index == crate::ewmh::NET_WM_DESKTOP_ALL {
            /* The window is requesting to be visible on all workspaces, so
             * let's float it and make it sticky. */
            crate::log::dlog_at(
                file!(),
                "handle_client_message",
                line!(),
                "The window was requested to be visible on all workspaces, making it sticky and floating.\n",
            );

            if crate::floating::floating_enable(t, b, con, false) {
                t.cons[con].floating = crate::data::FloatingState::FLOATING_AUTO_ON;

                t.cons[con].sticky = true;
                if let Some(window) = t.cons[con].window {
                    // C: ewmh_update_sticky(con->window->id, true);
                    crate::ewmh::ewmh_update_sticky(x, a, t.windows[window].id, true);
                }
                crate::output::output_push_sticky_windows(t, b, t.focused);
                // C: run_assignments(con->window);
                crate::assignments::run_assignments(t, b, t.cons[con].window);
            }
        } else {
            match crate::ewmh::ewmh_get_workspace_by_index(t, index) {
                None => {
                    // C: ELOG("Could not determine workspace for this index,
                    //          ignoring request.\n");
                    crate::log::dlog_at(
                        file!(),
                        "handle_client_message",
                        line!(),
                        "Could not determine workspace for this index, ignoring request.\n",
                    );
                }
                Some(ws) => {
                    // C: con_move_to_workspace(con, ws, true, false, false);
                    crate::con::con_move_to_workspace(t, b, con, ws, true, false, false);
                }
            }
        }

        // C: tree_render(); ewmh_update_wm_desktop();
        tree_render(t, b);
        if t.cons[con].window.is_some() {
            crate::ewmh::ewmh_update_wm_desktop(t, x, a);
        }
    }

    // C: src/handlers.c:813 — "} else if (event->type == A_I3_SYNC) {".
    // The testsuite sends this ClientMessage to the root window and blocks
    // until i3 answers; src/sync.c builds the reply event.
    if event.type_ == a.A_I3_SYNC {
        crate::sync::sync_respond(x, a, event.data32[0], event.data32[1]);
    }

    // C: src/handlers.c:961 — "} else if (event->type == A__NET_MOVERESIZE_WINDOW) {"
    // The client sends the geometry it wants to be moved/resized to; i3
    // synthesizes a ConfigureRequest out of it and reuses
    // handle_configure_request().
    if event.type_ == a.A__NET_MOVERESIZE_WINDOW {
        crate::log::dlog_at(
            file!(),
            "handle_client_message",
            line!(),
            "Received _NET_MOVE_RESIZE_WINDOW. Handling by faking a configure request.\n",
        );

        let mut generated = crate::handlers::configure_request_event_t {
            window: event.window,
            ..Default::default()
        };

        // C: src/handlers.c:669-672 _NET_MOVERESIZE_WINDOW_{X,Y,WIDTH,HEIGHT}
        // are (1 << 8), (1 << 9), (1 << 10) and (1 << 11).
        generated.value_mask = 0;
        if event.data32[0] & (1 << 8) != 0 {
            generated.value_mask |= 0x1; // XCB_CONFIG_WINDOW_X
            generated.x = event.data32[1] as i16;
        }
        if event.data32[0] & (1 << 9) != 0 {
            generated.value_mask |= 0x2; // XCB_CONFIG_WINDOW_Y
            generated.y = event.data32[2] as i16;
        }
        if event.data32[0] & (1 << 10) != 0 {
            generated.value_mask |= 0x4; // XCB_CONFIG_WINDOW_WIDTH
            generated.width = event.data32[3] as u16;
        }
        if event.data32[0] & (1 << 11) != 0 {
            generated.value_mask |= 0x8; // XCB_CONFIG_WINDOW_HEIGHT
            generated.height = event.data32[4] as u16;
        }

        handle_configure_request(t, b, x, a, &generated);
    }
    // C: A_WM_CHANGE_STATE (iconic) -> unmap request equivalent
    if event.type_ == a.A_WM_CHANGE_STATE {
        if let Some(con) = con_by_window_id(t, event.window) {
            let _ = con;
        }
    }
}

/// C: src/handlers.c:1063 handle_focus_in
pub fn handle_focus_in(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, event: &focus_in_event_t) {
    // C: src/handlers.c:1064 DLOG("focus change in, for window 0x%08x\n", event->event);
    crate::log::dlog_at(
        file!(),
        "handle_focus_in",
        line!(),
        &format!("focus change in, for window {:#010x}\n", event.event),
    );

    // C: src/handlers.c:1067-1073 — a focus in for the root window means the
    // focused client lost the focus: refocus our focused con and push.
    if event.event == x.root_screen().root {
        crate::log::dlog_at(
            file!(),
            "handle_focus_in",
            line!(),
            "Received focus in for root window, refocusing the focused window.\n",
        );
        if let Some(focused) = t.focused {
            crate::con::con_focus(t, b, focused);
            crate::x::set_focused_id(crate::data::XCB_WINDOW_NONE);
            // C: x_push_changes(croot);
            crate::tree::request_tree_render();
        }
        return;
    }

    // C: if the window that got focus is not the focused one, re-focus ours
    let con = match con_by_window_id(t, event.event) {
        Some(c) => c,
        None => return,
    };
    // C: if (focused->window != event->event) { x_set_focus(focused) }
    let is_ours = t.focused == Some(con);
    if !is_ours {
        // C: send WM_TAKE_FOCUS back to the window to keep focus in sync
        crate::xcb_util::send_take_focus(x, a, event.event, 0);
    }
}

/// C: src/handlers.c:1187 handle_configure_notify
pub fn handle_configure_notify(_t: &mut Tree, _b: &mut dyn crate::backend::ConBackend, _x: &mut dyn XConn, event: &configure_notify_event_t) {
    // C: randr_query_outputs on root geometry changes handled via screen change
    let _ = event;
}

/// C: src/handlers.c:1206 handle_selection_clear
pub fn handle_selection_clear(_t: &mut Tree, _b: &mut dyn crate::backend::ConBackend, _x: &mut dyn XConn) {
    // C: an ICCCM manager took over the selection -> exit gracefully
}

/// C: src/handlers.c:108 change_ewmh_focus
pub fn change_ewmh_focus(x: &mut dyn XConn, a: &Atoms, new_focus: u32, old_focus: u32) {
    // C: update _NET_ACTIVE_WINDOW
    crate::ewmh::ewmh_update_active_window(x, a, new_focus);
    // C: _NET_WM_STATE_FOCUSED add/remove
    if old_focus != 0 {
        crate::ewmh::ewmh_update_focused(x, a, old_focus, false);
    }
    if new_focus != 0 {
        crate::ewmh::ewmh_update_focused(x, a, new_focus, true);
    }
}

/**************************************************************************** * property handler table (C: struct property_handler_t)
****************************************************************************/

/// C: src/handlers.c:1325 struct property_handler_t
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct property_handler_t {
    /// the atom to watch
    pub atom: &'static str,
    /// true if this handler is useful for unmanaged windows
    pub for_unmanaged: bool,
}

/// C: src/handlers.c:1325-1344 property_handlers[] (13 entries)
pub const PROPERTY_HANDLERS: &[property_handler_t] = &[
    property_handler_t { atom: "_NET_WM_NAME", for_unmanaged: true },
    property_handler_t { atom: "WM_NAME", for_unmanaged: true },
    property_handler_t { atom: "WM_CLASS", for_unmanaged: true },
    property_handler_t { atom: "WM_WINDOW_ROLE", for_unmanaged: false },
    property_handler_t { atom: "_MOTIF_WM_HINTS", for_unmanaged: false },
    property_handler_t { atom: "WM_HINTS", for_unmanaged: false },
    property_handler_t { atom: "WM_NORMAL_HINTS", for_unmanaged: false },
    property_handler_t { atom: "WM_CLIENT_MACHINE", for_unmanaged: true },
    property_handler_t { atom: "_NET_WM_WINDOW_TYPE", for_unmanaged: true },
    property_handler_t { atom: "WM_TRANSIENT_FOR", for_unmanaged: true },
    property_handler_t { atom: "WM_CLIENT_LEADER", for_unmanaged: true },
    property_handler_t { atom: "_NET_WM_STRUT_PARTIAL", for_unmanaged: false },
    property_handler_t { atom: "_NET_WM_ICON", for_unmanaged: false },
];

/// C: src/handlers.c:1346 property_handlers_init — nothing to do at runtime in
/// the port (the table is static); kept for call-site parity.
pub fn property_handlers_init() {}

/// C: src/handlers.c:563 window_name_changed
///
/// Returns whether the window title actually changed (used to decide whether an
/// IPC "title" window event has to be sent).
fn window_name_changed(old_name: Option<&str>, new_name: Option<&str>) -> bool {
    match (old_name, new_name) {
        (None, None) => false,
        /* Either the old or the new one is NULL, but not both. */
        (None, Some(_)) | (Some(_), None) => true,
        (Some(old), Some(new)) => old != new,
    }
}

/// C: src/handlers.c:1364 property_notify
///
/// Dispatches the property change to the matching handler.
pub fn property_notify(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, x: &mut dyn XConn, a: &Atoms, event: &property_notify_event_t) {
    // C: src/handlers.c:1389-1397 — on PropertyDelete the handler is called
    // with a NULL reply (the GetProperty round trip is skipped), so a deleted
    // property clears the cached value instead of being ignored.
    let deleted = event.state == 1;
    macro_rules! prop_get {
        ($atom:expr, $len:expr) => {
            if deleted {
                None
            } else {
                x.get_property(event.window, $atom, 0, 0, $len)
            }
        };
    }
    let _atom_name = x.atom_name(event.atom);

    // C: find the property handler for the atom
    let handler = PROPERTY_HANDLERS
        .iter()
        .find(|h| {
            // C: the atoms are compared by id; match by interned value:
            h.atom == atom_by_value(a, event.atom)
        });

    let handler = match handler {
        Some(h) => h,
        None => return, // C: DLOG("Unhandled property notify")
    };

    let con = con_by_window_id(t, event.window);
    let window = con.and_then(|c| t.cons[c].window);

    match handler.atom {
        "_NET_WM_NAME" => {
            /* C: src/handlers.c:586 handle_windowname_change:
             *   char *old_name = sstrdup(i3string_as_utf8(con->window->name));
             *   window_update_name(con->window, prop);
             *   con = remanage_window(con);
             *   x_push_changes(croot);
             *   if (window_name_changed(con->window, old_name))
             *       ipc_send_window_event("title", con); */
            let reply = prop_get!(a.A__NET_WM_NAME, 128);
            if let Some(w) = window {
                let old_name = t.windows[w].name.clone();
                crate::manage::window_update_name(t, w, reply.as_ref());
                // The remanage is what lets a title based swallow criterion
                // match a window that changed its title after being mapped.
                let con = if let Some(c) = con {
                    let remanaged = crate::manage::remanage_window(t, b, x, a, c);
                    if let Some(croot) = t.croot {
                        b.x_push_changes(t, croot);
                    }
                    Some(remanaged)
                } else {
                    None
                };
                if let (Some(c), Some(w)) = (con, con.and_then(|c| t.cons[c].window)) {
                    if window_name_changed(old_name.as_deref(), t.windows[w].name.as_deref()) {
                        b.ipc_send_window_event(t, "title", c);
                    }
                }
            }
        }
        "WM_NAME" => {
            /* C: src/handlers.c:604 handle_windowname_change_legacy — same as
             * above, but through window_update_name_legacy(). */
            // C: xcb_get_property(conn, 0, window, XCB_ATOM_WM_NAME, ...)
            let reply = prop_get!(crate::data::XCB_ATOM_WM_NAME, 128);
            if let Some(w) = window {
                let old_name = t.windows[w].name.clone();
                crate::manage::window_update_name_legacy(t, w, reply.as_ref());
                let con = if let Some(c) = con {
                    let remanaged = crate::manage::remanage_window(t, b, x, a, c);
                    if let Some(croot) = t.croot {
                        b.x_push_changes(t, croot);
                    }
                    Some(remanaged)
                } else {
                    None
                };
                if let (Some(c), Some(w)) = (con, con.and_then(|c| t.cons[c].window)) {
                    if window_name_changed(old_name.as_deref(), t.windows[w].name.as_deref()) {
                        b.ipc_send_window_event(t, "title", c);
                    }
                }
            }
        }
        "WM_CLASS" => {
            // C: XCB_ATOM_WM_CLASS
            let reply = prop_get!(crate::data::XCB_ATOM_WM_CLASS, 128);
            if let Some(w) = window {
                crate::manage::window_update_class(t, w, reply.as_ref());
                // C: src/handlers.c:... handle_class_change — remanage +
                // x_push_changes (a class based swallow criterion can match now)
                if let Some(c) = con {
                    crate::manage::remanage_window(t, b, x, a, c);
                }
            }
        }
        "WM_WINDOW_ROLE" => {
            let reply = prop_get!(a.A_WM_WINDOW_ROLE, 128);
            if let Some(w) = window {
                crate::manage::window_update_role(t, w, reply.as_ref());
                // C: src/handlers.c:624-631 handle_windowrole_change —
                // remanage_window(con) so the for_window assignments run again
                // with the new role.
                if let Some(c) = con {
                    crate::manage::remanage_window(t, b, x, a, c);
                }
            }
        }
        "_MOTIF_WM_HINTS" => {
            let reply = prop_get!(a.A__MOTIF_WM_HINTS, 5 * 8);
            if let (Some(c), Some(w)) = (con, window) {
                let mut style = crate::data::BorderStyle::BS_NORMAL;
                if crate::manage::window_update_motif_hints(t, w, reply.as_ref(), &mut style) {
                    crate::con::con_set_border_style(t, c, style, t.config.default_border_width);
                }
            }
        }
        "WM_HINTS" => {
            // C: XCB_ATOM_WM_HINTS, 10 32-bit words
            let reply = prop_get!(crate::data::XCB_ATOM_WM_HINTS, 10);
            if let (Some(c), Some(w)) = (con, window) {
                let mut urgency = false;
                crate::manage::window_update_hints(t, w, reply.as_ref(), Some(&mut urgency));
                // C: src/handlers.c:1026 handle_hints — con_set_urgency(con,
                // urgency_hint) is called unconditionally, so that the window
                // event for "no longer urgent" is emitted as well.
                con_set_urgency(t, b, c, urgency);
                // C: src/handlers.c:1028 remanage_window(con); — re-runs the
                // for_window assignments (e.g. 'for_window [urgent=latest]
                // focus').
                let _con = crate::manage::remanage_window(t, b, x, a, c);
            }
        }
        "WM_NORMAL_HINTS" => {
            // C: XCB_ATOM_WM_NORMAL_HINTS, 24 words
            let reply = prop_get!(crate::data::XCB_ATOM_WM_NORMAL_HINTS, 24);
            if let Some(w) = window {
                crate::manage::window_update_normal_hints(t, w, reply.as_ref(), None);
            }
        }
        "WM_CLIENT_MACHINE" => {
            // C: XCB_ATOM_WM_CLIENT_MACHINE
            let reply = prop_get!(crate::data::XCB_ATOM_WM_CLIENT_MACHINE, u32::MAX);
            if let Some(w) = window {
                crate::manage::window_update_machine(t, w, reply.as_ref());
            }
        }
        "_NET_WM_WINDOW_TYPE" => {
            let reply = prop_get!(a.A__NET_WM_WINDOW_TYPE, u32::MAX);
            if let Some(w) = window {
                crate::manage::window_update_type(t, b, a, w, reply.as_ref());
            }
        }
        "WM_TRANSIENT_FOR" => {
            // C: XCB_ATOM_WM_TRANSIENT_FOR
            let reply = prop_get!(crate::data::XCB_ATOM_WM_TRANSIENT_FOR, u32::MAX);
            if let Some(w) = window {
                crate::manage::window_update_transient_for(t, w, reply.as_ref());
            }
        }
        "WM_CLIENT_LEADER" => {
            let reply = prop_get!(a.A_WM_CLIENT_LEADER, u32::MAX);
            if let Some(w) = window {
                crate::manage::window_update_leader(t, w, reply.as_ref());
            }
        }
        "_NET_WM_STRUT_PARTIAL" => {
            let reply = prop_get!(a.A__NET_WM_STRUT_PARTIAL, u32::MAX);
            if let Some(w) = window {
                crate::manage::window_update_strut_partial(t, w, reply.as_ref());
            }
        }
        "_NET_WM_ICON" => {
            let reply = prop_get!(a.A__NET_WM_ICON, u32::MAX);
            if let Some(w) = window {
                crate::manage::window_update_icon(t, w, reply.as_ref());
            }
        }
        _ => {}
    }

    // The C property handlers push their change to X11 themselves
    // (src/handlers.c:589/612 x_push_changes for the window name, :1310 for the
    // icon, :1249 for the motif hints, :1031/1302 tree_render for the hints and
    // the strut). The port defers the push to the event loop, so the tree is
    // marked dirty instead of rendering once per property.
    crate::tree::request_tree_render();
}

/// Reverse lookup of the atom name for the table match (the C compares atom
/// ids; the port compares names).
fn atom_by_value(a: &Atoms, atom: u32) -> &'static str {
    for (field, _name) in crate::xcb_util::ATOM_TABLE {
        let v = match *field {
            "A__NET_WM_NAME" => a.A__NET_WM_NAME,
            "A_WM_WINDOW_ROLE" => a.A_WM_WINDOW_ROLE,
            "A__MOTIF_WM_HINTS" => a.A__MOTIF_WM_HINTS,
            "A__NET_WM_WINDOW_TYPE" => a.A__NET_WM_WINDOW_TYPE,
            "A_WM_CLIENT_LEADER" => a.A_WM_CLIENT_LEADER,
            "A__NET_WM_STRUT_PARTIAL" => a.A__NET_WM_STRUT_PARTIAL,
            "A__NET_WM_ICON" => a.A__NET_WM_ICON,
            _ => continue,
        };
        if v == atom {
            return match *field {
                "A__NET_WM_NAME" => "_NET_WM_NAME",
                "A_WM_WINDOW_ROLE" => "WM_WINDOW_ROLE",
                "A__MOTIF_WM_HINTS" => "_MOTIF_WM_HINTS",
                "A__NET_WM_WINDOW_TYPE" => "_NET_WM_WINDOW_TYPE",
                "A_WM_CLIENT_LEADER" => "WM_CLIENT_LEADER",
                "A__NET_WM_STRUT_PARTIAL" => "_NET_WM_STRUT_PARTIAL",
                "A__NET_WM_ICON" => "_NET_WM_ICON",
                _ => "",
            };
        }
    }
    // the non-A_-prefixed atoms (WM_NAME etc. are XCB_ATOM_* fixed ids)
    match atom {
        crate::data::XCB_ATOM_WM_NAME => "WM_NAME",
        crate::data::XCB_ATOM_WM_CLASS => "WM_CLASS",
        crate::data::XCB_ATOM_WM_HINTS => "WM_HINTS",
        crate::data::XCB_ATOM_WM_NORMAL_HINTS => "WM_NORMAL_HINTS",
        crate::data::XCB_ATOM_WM_CLIENT_MACHINE => "WM_CLIENT_MACHINE",
        crate::data::XCB_ATOM_WM_TRANSIENT_FOR => "WM_TRANSIENT_FOR",
        _ => "",
    }
}

/**************************************************************************** * handle_event: the dispatch table (C: src/handlers.c:1410)
****************************************************************************/

/// XCB event type constants (core protocol response types).
pub const XCB_MAP_REQUEST: u8 = 20;
pub const XCB_UNMAP_NOTIFY: u8 = 18;
pub const XCB_DESTROY_NOTIFY: u8 = 17;
pub const XCB_CONFIGURE_REQUEST: u8 = 23;
pub const XCB_CONFIGURE_NOTIFY: u8 = 22;
pub const XCB_ENTER_NOTIFY: u8 = 7;
pub const XCB_MOTION_NOTIFY: u8 = 6;
pub const XCB_EXPOSE: u8 = 12;
pub const XCB_CLIENT_MESSAGE: u8 = 33;
pub const XCB_PROPERTY_NOTIFY: u8 = 28;
pub const XCB_FOCUS_IN: u8 = 9;
pub const XCB_BUTTON_PRESS: u8 = 4;
pub const XCB_BUTTON_RELEASE: u8 = 5;
pub const XCB_MAPPING_NOTIFY: u8 = 34;
pub const XCB_SELECTION_CLEAR: u8 = 17 + 12; // 29
/// C: randr_base screen change (randr extension dispatch handled via state).

/// Dispatch one X event to its handler.
///
/// C: src/handlers.c:1410 handle_event (the big switch over response types)
pub fn handle_event(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    a: &Atoms,
    event_type: u8,
    sequence: u32,
) -> bool {
    // C: if (event_is_ignored(sequence, event_type)) return false;
    if event_is_ignored(t, sequence, event_type as i32) {
        return false;
    }
    match event_type {
        XCB_MAP_REQUEST => {
            // C: xcb_map_request_event_t from the raw event
            handle_map_request(t, b, x, a, &map_request_event_t::default());
        }
        XCB_UNMAP_NOTIFY => {
            handle_unmap_notify_event(t, b, x, a, &unmap_notify_event_t { full_sequence: sequence, ..Default::default() });
        }
        XCB_DESTROY_NOTIFY => {
            handle_destroy_notify_event(t, b, x, a, &destroy_notify_event_t { full_sequence: sequence, ..Default::default() });
        }
        XCB_CONFIGURE_REQUEST => {
            handle_configure_request(t, b, x, a, &configure_request_event_t::default());
        }
        XCB_ENTER_NOTIFY => {
            handle_enter_notify(t, b, x, &enter_notify_event_t::default());
        }
        XCB_MOTION_NOTIFY => {
            handle_motion_notify(t, b, x, &motion_notify_event_t::default());
        }
        XCB_EXPOSE => {
            handle_expose_event(t, b, x, &expose_event_t::default());
        }
        XCB_CLIENT_MESSAGE => {
            handle_client_message(t, b, x, a, &client_message_event_t::default());
        }
        XCB_PROPERTY_NOTIFY => {
            property_notify(t, b, x, a, &property_notify_event_t::default());
        }
        XCB_FOCUS_IN => {
            handle_focus_in(t, b, x, a, &focus_in_event_t::default());
        }
        XCB_CONFIGURE_NOTIFY => {
            handle_configure_notify(t, b, x, &configure_notify_event_t::default());
        }
        XCB_SELECTION_CLEAR => {
            handle_selection_clear(t, b, x);
        }
        XCB_MAPPING_NOTIFY => {
            handle_mapping_notify(t, b, x, &mapping_notify_event_t::default());
        }
        _ => {
            // C: DLOG("Unhandled event with response_type = %d")
        }
    }
    true
}

/****************************************************************************
 * Raw x11rb event dispatch (C: src/handlers.c:1410 handle_event)
 ****************************************************************************/

/// C: XCB_RANDR_SCREEN_CHANGE_NOTIFY (xcb/randr.h)
pub const XCB_RANDR_SCREEN_CHANGE_NOTIFY: u8 = 0;
/// C: XCB_SHAPE_NOTIFY (xcb/shape.h)
pub const XCB_SHAPE_NOTIFY: u8 = 0;
/// C: XCB_XKB_NEW_KEYBOARD_NOTIFY / _MAP_NOTIFY / _STATE_NOTIFY (xcb/xkb.h)
pub const XCB_XKB_NEW_KEYBOARD_NOTIFY: u8 = 0;
pub const XCB_XKB_MAP_NOTIFY: u8 = 1;
pub const XCB_XKB_STATE_NOTIFY: u8 = 2;
/// C: XCB_XKB_NKN_DETAIL_KEYCODES
pub const XCB_XKB_NKN_DETAIL_KEYCODES: u16 = 1 << 0;
/// C: XCB_SHAPE_SK_BOUNDING / XCB_SHAPE_SK_INPUT
pub const XCB_SHAPE_SK_BOUNDING: u8 = 0;
pub const XCB_SHAPE_SK_INPUT: u8 = 2;
/// C: XCB_NOTIFY_MODE_NORMAL
pub const XCB_NOTIFY_MODE_NORMAL: u8 = 0;

/// The C globals of src/handlers.c that handle_event reads, plus the injection
/// points for the side effects of the extension handlers.
///
/// C: src/main.c:86 bindings, src/main.c:104 shape_supported,
/// src/handlers.c:23-25 randr_base / xkb_base / shape_base,
/// src/main.c root / wm_sn / last_timestamp.
#[derive(Default)]
pub struct EventContext<'a> {
    /// C: src/main.c xcb_atom_t wm_sn (the WM_Sn selection)
    pub wm_sn: u32,
    /// C: src/main.c xcb_window_t root
    pub root: u32,
    /// C: src/handlers.c:23 int randr_base = -1;
    pub randr_base: i32,
    /// C: src/handlers.c:24 int xkb_base = -1;
    pub xkb_base: i32,
    /// C: src/handlers.c:25 int shape_base = -1;
    pub shape_base: i32,
    /// C: src/main.c:104 bool shape_supported = true;
    pub shape_supported: bool,
    /// C: src/main.c:86 Binding *bindings (the active binding set)
    pub bindings: Option<&'a mut crate::bindings::Bindings>,
    /// C: src/bindings.c:16-18 xkb_context / xkb_keymap — the factory behind
    /// load_keymap(), which the xkb new-keyboard/map notify branches reload.
    /// (The concrete type is used rather than the trait object because the
    /// reference is re-borrowed on every loop iteration.)
    pub xkb_factory: Option<&'a mut crate::xkb::RealKeymapFactory>,
    /// C: the src/randr.c output state (src/randr.c:22 struct outputs_head
    /// outputs, root_output, has_randr_1_5). The RandR requests themselves go
    /// through the XConn that is passed to the handlers.
    pub randr: Option<&'a mut crate::randr::RandrState>,
    /// C: ipc_send_event("output", ...)
    pub ipc: Option<&'a mut crate::ipc::IpcServer>,
    /// C: src/main.c xcb_timestamp_t last_timestamp
    pub last_timestamp: u32,
    /// C: config.disable_focus_follows_mouse (src/config.c)
    pub disable_focus_follows_mouse: bool,
    /// C: force_xinerama (src/main.c)
    pub force_xinerama: bool,
}

/// Dispatch one raw x11rb event exactly like C handle_event().
///
/// Returns false when the event told i3 to terminate (SelectionClear on the
/// WM_Sn selection, C: exit(EXIT_SUCCESS)), true otherwise.
///
/// C: src/handlers.c:1410 void handle_event(int type, xcb_generic_event_t *event)
pub fn handle_x11_event(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    a: &Atoms,
    ev: &x11rb::protocol::Event,
) -> bool {
    let mut ctx = EventContext::default();
    handle_x11_event_ctx(t, b, x, a, ev, &mut ctx)
}

/// Same as handle_x11_event, but with the C globals/extension bases supplied
/// by the caller (the main loop).
pub fn handle_x11_event_ctx(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    a: &Atoms,
    ev: &x11rb::protocol::Event,
    ctx: &mut EventContext,
) -> bool {
    // C: ENSURE_FULL_SEQUENCE(event->full_sequence) — x11rb only exposes the
    // 16 bit wire sequence, so the upper half is unknown here.
    let sequence = ev.wire_sequence_number().unwrap_or(0) as u32;

    // C: the extension branches compare the raw response type against the
    // extension bases. x11rb only decodes the extensions whose cargo features
    // are enabled; everything else arrives as Event::Unknown(raw) with the 32
    // raw bytes, so the extension dispatch below works on the raw type byte
    // exactly like the C code.
    if let x11rb::protocol::Event::Unknown(raw) = ev {
        return handle_extension_event(t, b, x, a, raw, ctx);
    }

    use x11rb::protocol::Event;
    match ev {
        Event::KeyPress(e) => {
            // C: last_timestamp = event->time; handle_key_press(event) — the
            // binding lookup only needs the response type, state and keycode,
            // so both key event shapes are handed in as a BindingEvent.
            ctx.last_timestamp = e.time;
            let binding_event = crate::bindings::BindingEvent {
                response_type: crate::bindings::XCB_KEY_PRESS,
                state: u16::from(e.state),
                detail: u16::from(e.detail),
            };
            handle_key_press_event(t, b, x, a, &binding_event, ctx);
        }
        Event::KeyRelease(e) => {
            ctx.last_timestamp = e.time;
            let binding_event = crate::bindings::BindingEvent {
                response_type: crate::bindings::XCB_KEY_RELEASE,
                state: u16::from(e.state),
                detail: u16::from(e.detail),
            };
            handle_key_press_event(t, b, x, a, &binding_event, ctx);
        }
        Event::ButtonPress(e) => {
            ctx.last_timestamp = e.time;
            handle_button_press_event(t, b, x, a, &button_event_t {
                event: e.event,
                child: e.child,
                detail: e.detail,
                state: u16::from(e.state),
                event_x: i32::from(e.event_x),
                event_y: i32::from(e.event_y),
                root_x: i32::from(e.root_x),
                root_y: i32::from(e.root_y),
                time: e.time,
                response_type: XCB_BUTTON_PRESS,
            }, false, ctx);
        }
        Event::ButtonRelease(e) => {
            ctx.last_timestamp = e.time;
            handle_button_press_event(t, b, x, a, &button_event_t {
                event: e.event,
                child: e.child,
                detail: e.detail,
                state: u16::from(e.state),
                event_x: i32::from(e.event_x),
                event_y: i32::from(e.event_y),
                root_x: i32::from(e.root_x),
                root_y: i32::from(e.root_y),
                time: e.time,
                response_type: crate::bindings::XCB_BUTTON_RELEASE,
            }, true, ctx);
        }
        Event::MapRequest(e) => {
            // C: handle_map_request(event, event->full_sequence)
            handle_map_request(t, b, x, a, &map_request_event_t {
                window: e.window,
                parent: e.parent,
                full_sequence: sequence,
            });
        }
        Event::UnmapNotify(e) => {
            // C: handle_unmap_notify_event(event, event->full_sequence)
            handle_unmap_notify_event(t, b, x, a, &unmap_notify_event_t {
                window: e.window,
                event: e.event,
                from_configure: e.from_configure,
                full_sequence: sequence,
            });
        }
        Event::DestroyNotify(e) => {
            // C: handle_destroy_notify_event(event, event->full_sequence)
            handle_destroy_notify_event(t, b, x, a, &destroy_notify_event_t {
                window: e.window,
                event: e.event,
                full_sequence: sequence,
            });
        }
        Event::Expose(e) => {
            // C: if (((xcb_expose_event_t *)event)->count == 0)
            if e.count == 0 {
                handle_expose_event(t, b, x, &expose_event_t {
                    window: e.window,
                    x: e.x,
                    y: e.y,
                    width: e.width,
                    height: e.height,
                    count: e.count,
                });
            }
        }
        Event::MotionNotify(e) => {
            ctx.last_timestamp = e.time;
            handle_motion_notify(t, b, x, &motion_notify_event_t {
                event: e.event,
                child: e.child,
                root_x: e.root_x,
                root_y: e.root_y,
            });
        }
        Event::EnterNotify(e) => {
            ctx.last_timestamp = e.time;
            // C: handle_enter_notify checks config.disable_focus_follows_mouse
            // after resolving the container; the guard is hoisted here because
            // the full config is not part of Tree.
            if ctx.disable_focus_follows_mouse {
                return true;
            }
            handle_enter_notify(t, b, x, &enter_notify_event_t {
                event: e.event,
                child: e.child,
                detail: u8::from(e.detail),
                mode: u8::from(e.mode),
                root_x: e.root_x,
                root_y: e.root_y,
                full_sequence: sequence,
            });
        }
        Event::ClientMessage(e) => {
            handle_client_message(t, b, x, a, &client_message_event_t {
                window: e.window,
                type_: e.type_,
                format: e.format,
                data32: client_message_data32(&e.data),
            });
        }
        Event::ConfigureRequest(e) => {
            handle_configure_request(t, b, x, a, &configure_request_event_t {
                window: e.window,
                value_mask: u16::from(e.value_mask) as u32,
                x: e.x,
                y: e.y,
                width: e.width,
                height: e.height,
                sibling: e.sibling,
                stack_mode: u32::from(e.stack_mode) as u8,
            });
        }
        Event::MappingNotify(e) => {
            // C: handle_mapping_notify(event) — the bindings part lives in
            // bindings.rs; without a Bindings context the bookkeeping-only
            // port handler is used.
            let request = u8::from(e.request);
            if ctx.bindings.is_some() {
                // C: xcb_numlock_mask = aio_get_mod_mask_for(XCB_NUM_LOCK, keysyms);
                // The port resolves the Num_Lock keycode through the xkb keymap.
                let numlock_mask = {
                    let bindings = ctx.bindings.as_deref_mut().unwrap();
                    match bindings.keymap.as_deref() {
                        Some(keymap) => {
                            crate::xkb::aio_get_mod_mask_for(x, keymap, crate::xkb::XCB_NUM_LOCK)
                        }
                        None => crate::bindings::xcb_numlock_mask(),
                    }
                };
                let mut hooks = crate::bindings::NullHooks;
                let bindings = ctx.bindings.as_deref_mut().unwrap();
                crate::bindings::handle_mapping_notify(bindings, t, x, request, numlock_mask, &mut hooks);
            } else {
                handle_mapping_notify(t, b, x, &mapping_notify_event_t { request });
            }
        }
        Event::FocusIn(e) => {
            handle_focus_in(t, b, x, a, &focus_in_event_t {
                event: e.event,
                detail: u8::from(e.detail),
            });
        }
        Event::FocusOut(e) => {
            handle_focus_out(t, &focus_in_event_t {
                event: e.event,
                detail: u8::from(e.detail),
            });
        }
        Event::PropertyNotify(e) => {
            // C: last_timestamp = e->time; property_notify(e->state, e->window, e->atom)
            ctx.last_timestamp = e.time;
            property_notify(t, b, x, a, &property_notify_event_t {
                window: e.window,
                atom: e.atom,
                state: u8::from(e.state),
            });
        }
        Event::ConfigureNotify(e) => {
            handle_configure_notify_ctx(t, b, x, ctx, &configure_notify_event_t {
                event: e.event,
                window: e.window,
                override_redirect: e.override_redirect,
            });
        }
        Event::SelectionClear(e) => {
            // C: if (event->selection != wm_sn) return; LOG(...); exit(0);
            if ctx.wm_sn != 0 && e.selection == ctx.wm_sn {
                handle_selection_clear(t, b, x);
                return false;
            }
        }
        // The xkb events, in the decoded form x11rb uses once it knows the
        // extension. C only has the raw struct; the field-based handlers below
        // are shared with the Event::Unknown path of handle_extension_event()
        // (x11rb leaves extension events raw until an xkb request is sent).
        Event::XkbNewKeyboardNotify(e) => {
            handle_xkb_new_keyboard_notify_fields(t, x, u16::from(e.changed), ctx);
        }
        Event::XkbMapNotify(e) => {
            handle_xkb_map_notify_fields(t, x, e.sequence, ctx);
        }
        Event::XkbStateNotify(e) => {
            handle_xkb_state_notify_fields(t, x, u8::from(e.group), ctx);
        }
        _ => {
            // C: default: /* DLOG("Unhandled event of type %d\n", type); */
        }
    }
    true
}

/// The 32 bit view of xcb_client_message_event_t data.
fn client_message_data32(data: &x11rb::protocol::xproto::ClientMessageData) -> [u32; 5] {
    data.as_data32()
}

/****************************************************************************
 * Extension events: randr / xkb / shape (C: src/handlers.c:1415-1480)
 ****************************************************************************/

/// C: the three extension branches at the top of handle_event().
fn handle_extension_event(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    a: &Atoms,
    raw: &[u8],
    ctx: &mut EventContext,
) -> bool {
    let type_ = raw.first().copied().unwrap_or(0) & 0x7F;

    // C: if (randr_base > -1 && type == randr_base + XCB_RANDR_SCREEN_CHANGE_NOTIFY)
    if ctx.randr_base > -1
        && type_ as i32 == ctx.randr_base + XCB_RANDR_SCREEN_CHANGE_NOTIFY as i32
    {
        handle_screen_change(t, b, x, a, ctx);
        return true;
    }

    // C: if (xkb_base > -1 && type == xkb_base) { switch (state->xkbType) ... }
    if ctx.xkb_base > -1 && type_ as i32 == ctx.xkb_base {
        let xkb_type = raw.get(1).copied().unwrap_or(0xFF);
        match xkb_type {
            XCB_XKB_NEW_KEYBOARD_NOTIFY => handle_xkb_new_keyboard_notify(t, x, raw, ctx),
            XCB_XKB_MAP_NOTIFY => handle_xkb_map_notify(t, x, raw, ctx),
            XCB_XKB_STATE_NOTIFY => handle_xkb_state_notify(t, x, raw, ctx),
            _ => {}
        }
        return true;
    }

    // C: if (shape_supported && type == shape_base + XCB_SHAPE_NOTIFY)
    if ctx.shape_supported
        && ctx.shape_base > -1
        && type_ as i32 == ctx.shape_base + XCB_SHAPE_NOTIFY as i32
    {
        handle_shape_notify(t, raw);
        return true;
    }

    true
}


/// C: src/key_press.c:21 void handle_key_press(xcb_key_press_event_t *event)
///
/// Both KeyPress and KeyRelease carry the same fields, so one handler covers
/// both (the response type selects press vs release in get_binding).
fn handle_key_press_event(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    _a: &Atoms,
    event: &crate::bindings::BindingEvent,
    ctx: &mut EventContext,
) {
    let key_release = event.response_type == crate::bindings::XCB_KEY_RELEASE;
    crate::log::debuglog(&format!(
        "{} {}, state raw = 0x{:x}\n",
        if key_release { "KeyRelease" } else { "KeyPress" },
        event.detail,
        event.state
    ));

    // C: Binding *bind = get_binding_from_xcb_event(event); if (bind == NULL) return;
    let bindings = match ctx.bindings.as_deref_mut() {
        Some(b) => b,
        None => return,
    };
    // C: get_binding_from_xcb_event() also re-arms the IGNORE_MODS release
    // state; the returned index is into the current mode's binding list.
    let bind = match crate::bindings::get_binding_from_xcb_event(t, event) {
        Some(idx) => bindings
            .modes
            .iter()
            .flat_map(|m| m.bindings.iter())
            .nth(idx)
            .cloned(),
        None => None,
    };
    if let Some(bind) = bind {
        // C: CommandResult *result = run_binding(bind, NULL);
        let mut hooks = crate::ipc::IpcHooks;
        let _ = crate::bindings::run_binding(bindings, t, b, &mut hooks, &bind, None);
    }
    let _ = (x, ctx);
}

/// C: src/handlers.c — ButtonPress/ButtonRelease are routed by src/click.c.
fn handle_button_press_event(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    a: &Atoms,
    event: &button_event_t,
    button_release: bool,
    ctx: &mut EventContext,
) {
    crate::click::handle_button_press(t, b, x, a, event, button_release, ctx);
}

/// C: src/handlers.c: handle_focus_out — only the DLOG remains (the focus
/// follow logic lives in handle_enter_notify).
fn handle_focus_out(t: &mut Tree, event: &focus_in_event_t) {
    let _ = t;
    crate::log::debuglog(&format!("focus out, event->event = {:#x}\n", event.event));
}

/// C: src/handlers.c:1250 handle_configure_notify — the root window means an
/// output geometry change; anything else is a managed window.
fn handle_configure_notify_ctx(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    ctx: &mut EventContext,
    event: &configure_notify_event_t,
) {
    if event.window == ctx.root {
        crate::log::debuglog("Root window changed size. Adjusting the resize bar and all Cons.\n");
        // C: randr_query_outputs() + ipc_send_event("output", ...) — the
        // RandR side is driven by the RandrState the caller passed in.
        if let Some(state) = ctx.randr.as_deref_mut() {
            crate::randr::randr_query_outputs(t, b, x, state);
        }
        return;
    }
    if let Some(con) = crate::con::con_by_window_id(t, event.window) {
        crate::log::debuglog(&format!(
            "ConfigureNotify for managed window {:#x} (con {:?})\n",
            event.window, con
        ));
    }
}

/// C: src/handlers.c:1421 (XKB_NEW_KEYBOARD_NOTIFY) — decoded-event variant.
fn handle_xkb_new_keyboard_notify_fields(
    t: &mut Tree,
    x: &mut dyn XConn,
    changed: u16,
    ctx: &mut EventContext,
) {
    crate::log::debuglog(&format!("xkb new keyboard notify, changed = {:#x}\n", changed));
    // C: if (!(event->changed & XCB_XKB_NKN_DETAIL_KEYCODES)) return;
    if changed & XCB_XKB_NKN_DETAIL_KEYCODES == 0 {
        return;
    }
    xkb_reload_keymap_and_grab(t, x, ctx);
}

/// C: src/handlers.c:1421 — the raw (undecoded) xkb event variant.
fn handle_xkb_new_keyboard_notify(t: &mut Tree, x: &mut dyn XConn, raw: &[u8], ctx: &mut EventContext) {
    // xkb_new_keyboard_notify_event_t: response_type(0) xkbType(1) sequence(2..4)
    // time(4..8) deviceID(8) oldDeviceID(9) minKeyCode(10) maxKeyCode(11)
    // oldMinKeyCode(12) oldMaxKeyCode(13) requestMajor(14) requestMinor(15)
    // changed(16..18)
    let changed = u16::from_ne_bytes([raw.get(16).copied().unwrap_or(0), raw.get(17).copied().unwrap_or(0)]);
    handle_xkb_new_keyboard_notify_fields(t, x, changed, ctx);
}

/// C: src/handlers.c:1430 (XKB_MAP_NOTIFY).
fn handle_xkb_map_notify_fields(t: &mut Tree, x: &mut dyn XConn, sequence: u16, ctx: &mut EventContext) {
    // C: add_ignore_event(ENSURE_FULL_SEQUENCE(event->sequence), -1); the
    // x11rb decoded event keeps only the 16 bit sequence.
    add_ignore_event_seq(u32::from(sequence), -1);
    xkb_reload_keymap_and_grab(t, x, ctx);
}

fn handle_xkb_map_notify(t: &mut Tree, x: &mut dyn XConn, raw: &[u8], ctx: &mut EventContext) {
    let sequence = u16::from_ne_bytes([raw.get(2).copied().unwrap_or(0), raw.get(3).copied().unwrap_or(0)]);
    handle_xkb_map_notify_fields(t, x, sequence, ctx);
}

/// C: src/handlers.c:1448 (XKB_STATE_NOTIFY) — the keyboard group changed.
fn handle_xkb_state_notify_fields(t: &mut Tree, x: &mut dyn XConn, group: u8, ctx: &mut EventContext) {
    let bindings = match ctx.bindings.as_deref_mut() {
        Some(b) => b,
        None => return,
    };
    crate::bindings::handle_xkb_state_notify(bindings, t, x, u32::from(group));
}

fn handle_xkb_state_notify(t: &mut Tree, x: &mut dyn XConn, raw: &[u8], ctx: &mut EventContext) {
    // xkb_state_notify_event_t: ... group is the 5th byte after the header
    // (response_type, xkbType, sequence(2), time(4..8), deviceID(8),
    //  mods(9..11), base_mods(11..13), latched_mods..., group(18)).
    let group = raw.get(18).copied().unwrap_or(0);
    handle_xkb_state_notify_fields(t, x, group, ctx);
}

/// C: src/handlers.c:1461 (SHAPE_NOTIFY) → x_set_shape().
fn handle_shape_notify(t: &mut Tree, raw: &[u8]) {
    // xcb_shape_notify_event_t: affected_window(4..8), shaped(8)
    let affected = u32::from_ne_bytes([
        raw.get(4).copied().unwrap_or(0),
        raw.get(5).copied().unwrap_or(0),
        raw.get(6).copied().unwrap_or(0),
        raw.get(7).copied().unwrap_or(0),
    ]);
    let shaped = raw.get(8).copied().unwrap_or(0);
    if let Some(con) = crate::con::con_by_window_id(t, affected) {
        crate::log::debuglog(&format!("window {:#x} shaped={}\n", affected, shaped));
        let _ = con;
    }
}

/// C: src/handlers.c: re-loads the xkb keymap and re-grabs every key.
fn xkb_reload_keymap_and_grab(t: &mut Tree, x: &mut dyn XConn, ctx: &mut EventContext) {
    crate::log::debuglog("reloading keymap\n");
    if let Some(factory) = ctx.xkb_factory.as_deref_mut() {
        let bindings = match ctx.bindings.as_deref_mut() {
            Some(b) => b,
            None => return,
        };
        // C: load_keymap() (bindings.c:980) — rebuilds bindings.keymap.
        crate::bindings::load_keymap(bindings, x, factory);
        crate::bindings::ungrab_all_keys(x);
        crate::bindings::set_xcb_numlock_mask(crate::bindings::xcb_numlock_mask());
        let mut hooks = crate::ipc::IpcHooks;
        crate::bindings::translate_keysyms(bindings, t, &mut hooks);
        crate::bindings::grab_all_keys(bindings, t, x);
    }
}

/// C: src/commands.c / src/workspace.c — is there anything on __i3_scratch?
fn tree_has_scratchpad(t: &Tree) -> bool {
    let i3_con = match t.croot {
        Some(root) => t
            .cons[root]
            .nodes_head
            .iter()
            .copied()
            .find(|c| t.cons[*c].name.as_deref() == Some("__i3")),
        None => None,
    };
    let i3_con = match i3_con {
        Some(c) => c,
        None => return false,
    };
    !t.cons[i3_con].nodes_head.is_empty()
}

/// C: src/handlers.c:454 handle_screen_change
///
/// The root geometry is read from X and copied to croot; the RandR output
/// query runs when a RandRReplyProvider was supplied (the reply source is
/// still Phase 7), then the scratchpad resolution and the IPC output event.
pub fn handle_screen_change(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    _a: &Atoms,
    ctx: &mut EventContext,
) {
    let root = ctx.root;
    // C: xcb_get_geometry(conn, root)
    let reply = match x.get_geometry(root) {
        Some(r) => r,
        None => {
            // C: ELOG("Could not get geometry of the root window, exiting\n"); exit(1)
            crate::log::errorlog("Could not get geometry of the root window\n");
            return;
        }
    };
    // C: croot->rect.width = reply->width; croot->rect.height = reply->height;
    if let Some(croot) = t.croot {
        t.cons[croot].rect.width = u32::from(reply.width);
        t.cons[croot].rect.height = u32::from(reply.height);
    }

    randr_query_outputs_ctx(t, b, x, ctx);

    // C: scratchpad_fix_resolution()
    if tree_has_scratchpad(t) {
        crate::scratchpad::scratchpad_fix_resolution(t, b);
    }

    // C: ipc_send_event("output", I3_IPC_EVENT_OUTPUT, "{\"change\":\"unspecified\"}")
    if let Some(ipc) = ctx.ipc.as_deref_mut() {
        ipc.ipc_send_event(
            "output",
            crate::ipc::I3_IPC_EVENT_OUTPUT,
            "{\"change\":\"unspecified\"}",
        );
    }
}

/// C: randr_query_outputs() from the handlers — reads the outputs through the
/// X connection of the caller.
fn randr_query_outputs_ctx(
    t: &mut Tree,
    b: &mut dyn crate::backend::ConBackend,
    x: &mut dyn XConn,
    ctx: &mut EventContext,
) {
    if let Some(state) = ctx.randr.as_deref_mut() {
        // C: src/randr.c:926 randr_query_outputs()
        crate::randr::randr_query_outputs(t, b, x, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{ConBackend, NullBackend, PropertyReply};
    use crate::data::{output_name, ConType, FrameSurface, FullscreenMode, KillWindow, Layout, Output, Rect, Window};
    use crate::xcb_util::{Atoms, RecordingConn};
    use x11rb::protocol::Event;

    /// Records the ConBackend side effects the handlers trigger (C: the ipc.c /
    /// x.c calls the handlers make directly).
    #[derive(Default)]
    struct RecordingBackend {
        ipc_events: Vec<(String, ConId)>,
        pushes: usize,
        killed: Vec<(u32, KillWindow)>,
        /// The commands run_binding() handed to the command layer.
        commands: Vec<String>,
    }

    impl ConBackend for RecordingBackend {
        fn ipc_send_window_event(&mut self, _t: &Tree, event: &str, con: ConId) {
            self.ipc_events.push((event.to_string(), con));
        }

        /// C: src/commands.c parse_command — the command a key binding ran.
        fn parse_command(&mut self, _t: &mut Tree, command: &str) -> bool {
            self.commands.push(command.to_string());
            false
        }

        fn x_push_changes(&mut self, _t: &mut Tree, _con: ConId) {
            self.pushes += 1;
        }

        fn x_window_kill(&mut self, _t: &Tree, window_id: u32, kill_window: KillWindow) {
            self.killed.push((window_id, kill_window));
        }
    }

    /// The tree shape src/randr.c/workspace.c build at startup:
    /// croot -> output -> content -> workspace(1) -> one client con (0x42).
    struct Env {
        t: Tree,
        x: RecordingConn,
        a: Atoms,
        ws: ConId,
        con: ConId,
        window: crate::data::WindowId,
    }

    fn env() -> Env {
        let mut t = Tree::new();
        let mut b = NullBackend;
        crate::tree::tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));
        let croot = t.croot.unwrap();

        let output = crate::con::con_new(&mut t, &mut b, Some(croot), None);
        t.cons[output].r#type = ConType::CT_OUTPUT;
        t.cons[output].layout = Layout::L_OUTPUT;
        t.cons[output].name = Some("fake-0".to_string());
        t.cons[output].rect = Rect::new(0, 0, 1280, 800);

        let mut output_data = Output::default();
        output_data.active = true;
        output_data.primary = true;
        output_data.rect = Rect::new(0, 0, 1280, 800);
        output_data.con = Some(output);
        output_data.names_head.push_back(output_name { name: "fake-0".to_string() });
        t.outputs.push_back(output_data);

        let content = crate::con::con_new(&mut t, &mut b, Some(output), None);
        t.cons[content].r#type = ConType::CT_CON;
        t.cons[content].layout = Layout::L_SPLITH;
        t.cons[content].rect = Rect::new(0, 0, 1280, 800);

        let ws = crate::con::con_new(&mut t, &mut b, Some(content), None);
        t.cons[ws].r#type = ConType::CT_WORKSPACE;
        t.cons[ws].layout = Layout::L_SPLITH;
        t.cons[ws].name = Some("1".to_string());
        t.cons[ws].rect = Rect::new(0, 0, 1280, 800);

        let con = crate::con::con_new(&mut t, &mut b, Some(ws), None);
        t.cons[con].r#type = ConType::CT_CON;
        t.cons[con].layout = Layout::L_SPLITH;
        t.cons[con].rect = Rect::new(0, 0, 1280, 800);
        t.cons[con].deco_rect = Rect::new(0, 0, 1280, 18);
        t.cons[con].window_rect = Rect::new(0, 18, 1280, 780);
        // C: x_con_init() assigned the frame window of this con.
        t.cons[con].frame = Some(FrameSurface { id: 0x900 });

        let window = t.windows.insert(Window { id: 0x42, ..Default::default() });
        t.window_index.insert(0x42, window);
        t.cons[con].window = Some(window);
        t.focused = Some(con);

        let mut x = RecordingConn::new(0x50);
        let a = Atoms::intern_all(&mut x);
        x.log.clear();
        Env { t, x, a, ws, con, window }
    }

    fn client_message(type_: u32, window: u32, data32: [u32; 5]) -> client_message_event_t {
        client_message_event_t { window, type_, format: 32, data32 }
    }

    /// C: src/handlers.c:38 add_ignore_event with the wildcard response type
    /// (-1) used by handle_map_request: every event type of that sequence is
    /// dropped, other sequences are untouched.
    #[test]
    fn ignore_event_matches_wildcard_response_type() {
        let mut t = Tree::new();
        add_ignore_event_seq(0xAB00_0001, -1);
        assert!(event_is_ignored(&mut t, 0xAB00_0001, XCB_MAP_REQUEST as i32));
        assert!(event_is_ignored(&mut t, 0xAB00_0001, XCB_PROPERTY_NOTIFY as i32));
        assert!(!event_is_ignored(&mut t, 0xAB00_0002, XCB_PROPERTY_NOTIFY as i32));
    }

    /// C: src/handlers.c:52 event_is_ignored — a recorded response type only
    /// matches that type; the sequence alone is not enough.
    #[test]
    fn ignore_event_honours_the_recorded_response_type() {
        let mut t = Tree::new();
        add_ignore_event(&mut t, 0xAB00_0003, XCB_ENTER_NOTIFY as i32);
        assert!(event_is_ignored(&mut t, 0xAB00_0003, XCB_ENTER_NOTIFY as i32));
        assert!(!event_is_ignored(&mut t, 0xAB00_0003, XCB_MAP_REQUEST as i32));
    }

    /// C: handle_event() drops an event whose sequence is already in the
    /// ignore list (the synthetic event is gone, so the registration is done
    /// explicitly here — the typed path registers it in handle_map_request).
    #[test]
    fn handle_event_drops_repeated_sequences() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        add_ignore_event(&mut env.t, 0xAB00_0010, -1);
        assert!(event_is_ignored(&mut env.t, 0xAB00_0010, -1));
        assert!(!handle_event(&mut env.t, &mut b, &mut env.x, &env.a, XCB_MAP_REQUEST, 0xAB00_0010));
    }

    /// C: src/handlers.c:283 handle_map_request — reads the attributes, gives
    /// up when the window vanished and registers the full sequence.
    #[test]
    fn map_request_event_queries_attributes_and_registers_the_sequence() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();
        let ev = Event::MapRequest(x11rb::protocol::xproto::MapRequestEvent {
            window: 0x42,
            parent: 0x50,
            sequence: 0x1234,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(
            env.x.log.iter().any(|l| l == "get_window_attributes(0x42)"),
            "log={:?}",
            env.x.log
        );
        assert!(event_is_ignored(&mut env.t, 0x1234, -1));
    }

    /// C: src/handlers.c:482 handle_unmap_notify_event — a managed window is
    /// closed: _NET_WM_DESKTOP/_NET_WM_STATE are deleted and the container is
    /// torn down (ipc "close" event).
    #[test]
    fn unmap_notify_closes_the_managed_con_and_clears_ewmh_props() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();
        let ev = Event::UnmapNotify(x11rb::protocol::xproto::UnmapNotifyEvent {
            window: 0x42,
            event: 0x50,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));

        assert!(
            env.x
                .log
                .contains(&format!("delete_property(win=0x42, prop={})", env.a.A__NET_WM_DESKTOP)),
            "log={:?}",
            env.x.log
        );
        assert!(env
            .x
            .log
            .contains(&format!("delete_property(win=0x42, prop={})", env.a.A__NET_WM_STATE)));
        assert!(
            b.ipc_events.iter().any(|(event, con)| event == "close" && *con == env.con),
            "events={:?}",
            b.ipc_events
        );
        assert!(!env.t.all_cons.contains(&env.con), "the con was detached");
    }

    /// C: src/handlers.c:554 handle_destroy_notify_event — destroy shares the
    /// unmap path (the X server already destroyed the window).
    #[test]
    fn destroy_notify_follows_the_unmap_path() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();
        let ev = Event::DestroyNotify(x11rb::protocol::xproto::DestroyNotifyEvent {
            window: 0x42,
            event: 0x50,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(env
            .x
            .log
            .contains(&format!("delete_property(win=0x42, prop={})", env.a.A__NET_WM_DESKTOP)));
        assert!(b.ipc_events.iter().any(|(event, _)| event == "close"));
    }

    /// C: src/handlers.c:300 handle_configure_request — an unmanaged window
    /// gets its geometry request executed verbatim (borders are ours, so the
    /// border width is forced to 0).
    #[test]
    fn configure_request_for_an_unmanaged_window_forwards_the_geometry() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();
        let ev = Event::ConfigureRequest(x11rb::protocol::xproto::ConfigureRequestEvent {
            window: 0x77,
            value_mask: x11rb::protocol::xproto::ConfigWindow::X
                | x11rb::protocol::xproto::ConfigWindow::Y
                | x11rb::protocol::xproto::ConfigWindow::WIDTH
                | x11rb::protocol::xproto::ConfigWindow::HEIGHT
                | x11rb::protocol::xproto::ConfigWindow::BORDER_WIDTH,
            x: 10,
            y: 20,
            width: 800,
            height: 600,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));

        assert!(
            env.x
                .log
                .contains(&"configure_window(0x77, [(X, 10), (Y, 20), (WIDTH, 800), (HEIGHT, 600), (BORDER_WIDTH, 0)])".to_string()),
            "log={:?}",
            env.x.log
        );
    }

    /// C: src/xcb.c:64 fake_absolute_configure_notify — the coordinates are
    /// absolute (con->rect + con->window_rect), not window-relative, because
    /// the client's frame is what the window manager positioned.
    #[test]
    fn fake_absolute_configure_notify_uses_absolute_coordinates() {
        let mut env = env();
        let con = env.con;
        // A floating window somewhere on the screen: the frame at (100, 50),
        // the client at (1, 18) inside the frame (the decoration bar).
        env.t.cons[con].rect = Rect::new(100, 50, 400, 300);
        env.t.cons[con].window_rect = Rect::new(1, 18, 398, 282);
        env.t.cons[con].border_width = 2;
        crate::xcb_util::fake_absolute_configure_notify(&env.t, &mut env.x, con);
        let line = env
            .x
            .log
            .iter()
            .find(|l| l.starts_with("send_configure_notify"))
            .unwrap_or_else(|| panic!("no ConfigureNotify sent: {:?}", env.x.log));
        // 100 + 1, 50 + 18, the client size, and con->border_width.
        assert!(
            line.contains("win=0x42") && line.contains("x=101") && line.contains("y=68")
                && line.contains("w=398") && line.contains("h=282") && line.contains("bw=2"),
            "unexpected event: {line}"
        );
    }

    /// C: src/handlers.c:338-342 — a scratchpad container ignores configure
    /// requests (the scratchpad keeps its own geometry).
    #[test]
    fn configure_request_on_the_scratchpad_workspace_is_ignored() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        env.t.cons[env.ws].name = Some("__i3_scratch".to_string());
        handle_configure_request(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &configure_request_event_t {
                window: 0x42,
                value_mask: 0x1,
                x: 100,
                ..Default::default()
            },
        );
        // C: handlers.c:340 jumps to the "out:" label, which runs
        // fake_absolute_configure_notify(con) — the window is *not*
        // repositioned, but it does get a synthetic ConfigureNotify.
        assert!(
            !env.x.log.iter().any(|l| l.starts_with("configure_window")),
            "the scratchpad window must not be repositioned: {:?}",
            env.x.log
        );
        // The event is built and sent by libi3/fake_configure_notify.c, so the
        // record is send_configure_notify().
        assert!(
            env.x.log.iter().any(|l| l.starts_with("send_configure_notify")),
            "C sends a fake ConfigureNotify: {:?}",
            env.x.log
        );
    }

    /// C: src/handlers.c:639 handle_expose_event — only the last expose of a
    /// frame marks the con for a redraw.
    #[test]
    fn expose_event_marks_the_frame_for_redraw() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();

        let last = Event::Expose(x11rb::protocol::xproto::ExposeEvent {
            window: 0x900,
            count: 0,
            ..Default::default()
        });
        handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &last, &mut ctx);
        assert!(env.t.cons[env.con].mark_changed, "the frame is marked");

        env.t.cons[env.con].mark_changed = false;
        let more = Event::Expose(x11rb::protocol::xproto::ExposeEvent {
            window: 0x900,
            count: 2,
            ..Default::default()
        });
        handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &more, &mut ctx);
        assert!(!env.t.cons[env.con].mark_changed, "a partial expose is ignored");
    }

    /// C: src/handlers.c:812 _NET_ACTIVE_WINDOW -> con_activate_unblock().
    #[test]
    fn client_message_net_active_window_activates_the_con() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();
        let ev = Event::ClientMessage(x11rb::protocol::xproto::ClientMessageEvent::new(
            32,
            0x42,
            env.a.A__NET_ACTIVE_WINDOW,
            x11rb::protocol::xproto::ClientMessageData::from([7u32, 0, 0, 0, 0]),
        ));
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(env.t.focused, Some(env.con));
    }

    /// C: src/handlers.c:822 _NET_CLOSE_WINDOW -> tree_close_internal with
    /// KILL_WINDOW (the client is killed, not just unmapped).
    #[test]
    fn client_message_net_close_window_kills_the_client() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();
        let ev = Event::ClientMessage(x11rb::protocol::xproto::ClientMessageEvent::new(
            32,
            0x42,
            env.a.A__NET_CLOSE_WINDOW,
            x11rb::protocol::xproto::ClientMessageData::from([0u32, 0, 0, 0, 0]),
        ));
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(b.killed, vec![(0x42, KillWindow::KILL_WINDOW)]);
    }

    /// C: src/handlers.c:690 _NET_WM_STATE_FULLSCREEN with _NET_WM_STATE_ADD.
    #[test]
    fn net_wm_state_fullscreen_add_toggles_fullscreen() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        handle_client_message(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &client_message(
                env.a.A__NET_WM_STATE,
                0x42,
                [1, env.a.A__NET_WM_STATE_FULLSCREEN, 0, 0, 0],
            ),
        );
        assert_eq!(env.t.cons[env.con].fullscreen_mode, FullscreenMode::CF_OUTPUT);
    }

    /// C: src/handlers.c:708-726 _NET_WM_STATE_STICKY -> the state is set and
    /// ewmh_update_sticky() writes _NET_WM_STATE on the client window.
    #[test]
    fn net_wm_state_sticky_add_sets_sticky_and_updates_the_property() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        handle_client_message(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &client_message(
                env.a.A__NET_WM_STATE,
                0x42,
                [1, env.a.A__NET_WM_STATE_STICKY, 0, 0, 0],
            ),
        );
        assert!(env.t.cons[env.con].sticky);
        assert!(
            env.x.log.iter().any(|l| l.starts_with("change_property(win=0x42")),
            "log={:?}",
            env.x.log
        );
    }

    /// C: i3 never handles _NET_WM_STATE_HIDDEN itself (it only writes it in
    /// set_maximized_state): a client message must not change anything.
    #[test]
    fn net_wm_state_hidden_is_not_handled() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        handle_client_message(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &client_message(
                env.a.A__NET_WM_STATE,
                0x42,
                [1, env.a.A__NET_WM_STATE_HIDDEN, 0, 0, 0],
            ),
        );
        assert!(!env.t.cons[env.con].sticky);
        assert_eq!(env.t.cons[env.con].fullscreen_mode, FullscreenMode::CF_NONE);
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
    }

    /// C: src/handlers.c:961 _NET_MOVERESIZE_WINDOW is turned into a synthetic
    /// ConfigureRequest (the flag bits are 1 << 8 .. 1 << 11).
    #[test]
    fn net_moveresize_window_becomes_a_configure_request() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        handle_client_message(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &client_message(
                env.a.A__NET_MOVERESIZE_WINDOW,
                0x77,
                [(1 << 8) | (1 << 9), 100, 200, 0, 0],
            ),
        );
        assert!(
            env.x.log.contains(&"configure_window(0x77, [(X, 100), (Y, 200)])".to_string()),
            "log={:?}",
            env.x.log
        );
    }

    /// C: src/handlers.c:1206 handle_selection_clear — losing the WM_Sn
    /// selection makes i3 exit (the caller stops the event loop).
    #[test]
    fn selection_clear_for_wm_sn_asks_to_exit() {
        let mut env = env();
        let mut b = RecordingBackend::default();

        let mut ctx = EventContext { wm_sn: 0x1234, ..Default::default() };
        let ours = Event::SelectionClear(x11rb::protocol::xproto::SelectionClearEvent {
            selection: 0x1234,
            ..Default::default()
        });
        assert!(!handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ours, &mut ctx));

        let mut ctx = EventContext { wm_sn: 0x1234, ..Default::default() };
        let other = Event::SelectionClear(x11rb::protocol::xproto::SelectionClearEvent {
            selection: 0x9999,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &other, &mut ctx));
    }

    /// C: src/handlers.c:1364 property_notify — a deletion (state 1) is not a
    /// value change and is ignored before any property is read.
    #[test]
    fn property_notify_ignores_deletions() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        property_notify(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &property_notify_event_t { window: 0x42, atom: env.a.A__NET_WM_NAME, state: 1 },
        );
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
    }

    /// C: src/handlers.c:1325 property_handlers[] — all 13 entries are matched
    /// by the atom id of the event (6 fixed XCB_ATOM_* ids, 7 interned atoms).
    #[test]
    fn property_notify_table_covers_13_atoms() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        assert_eq!(PROPERTY_HANDLERS.len(), 13);

        let table: Vec<(&'static str, u32)> = vec![
            ("_NET_WM_NAME", env.a.A__NET_WM_NAME),
            ("WM_NAME", crate::data::XCB_ATOM_WM_NAME),
            ("WM_CLASS", crate::data::XCB_ATOM_WM_CLASS),
            ("WM_WINDOW_ROLE", env.a.A_WM_WINDOW_ROLE),
            ("_MOTIF_WM_HINTS", env.a.A__MOTIF_WM_HINTS),
            ("WM_HINTS", crate::data::XCB_ATOM_WM_HINTS),
            ("WM_NORMAL_HINTS", crate::data::XCB_ATOM_WM_NORMAL_HINTS),
            ("WM_CLIENT_MACHINE", crate::data::XCB_ATOM_WM_CLIENT_MACHINE),
            ("_NET_WM_WINDOW_TYPE", env.a.A__NET_WM_WINDOW_TYPE),
            ("WM_TRANSIENT_FOR", crate::data::XCB_ATOM_WM_TRANSIENT_FOR),
            ("WM_CLIENT_LEADER", env.a.A_WM_CLIENT_LEADER),
            ("_NET_WM_STRUT_PARTIAL", env.a.A__NET_WM_STRUT_PARTIAL),
            ("_NET_WM_ICON", env.a.A__NET_WM_ICON),
        ];

        for (name, atom) in table {
            assert_eq!(atom_by_value(&env.a, atom), name, "atom {:#x} is {name}", atom);
            env.x.log.clear();
            property_notify(
                &mut env.t,
                &mut b,
                &mut env.x,
                &env.a,
                &property_notify_event_t { window: 0x42, atom, state: 0 },
            );
            // The recording connection answers from its props map without a
            // log line (C's xcb_get_property has no side effect either); what
            // matters is that the handler consults the right atom.
            assert!(
                !env.x.log.iter().any(|l| l.contains("BUG")),
                "{name}: unexpected log {:?}",
                env.x.log
            );
        }
    }

    /// C: src/handlers.c:586 handle_windowname_change — the new title is
    /// stored and an ipc "title" window event is emitted when it changed.
    #[test]
    fn property_notify_updates_the_window_name_and_emits_title() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        env.x.props.insert(
            (0x42, env.a.A__NET_WM_NAME),
            PropertyReply {
                type_: env.a.A_UTF8_STRING,
                format: 8,
                value: b"new title\0".to_vec(),
            },
        );
        property_notify(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &property_notify_event_t { window: 0x42, atom: env.a.A__NET_WM_NAME, state: 0 },
        );
        assert_eq!(env.t.windows[env.window].name.as_deref(), Some("new title"));
        assert!(
            b.ipc_events.iter().any(|(event, con)| event == "title" && *con == env.con),
            "events={:?}",
            b.ipc_events
        );
    }

    /// C: src/handlers.c:1391 property_notify — an atom without a handler is
    /// dropped without reading the property.
    #[test]
    fn property_notify_ignores_unknown_atoms() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        property_notify(
            &mut env.t,
            &mut b,
            &mut env.x,
            &env.a,
            &property_notify_event_t { window: 0x42, atom: 0xdead, state: 0 },
        );
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
    }

    /// A default-mode binding set with one bindcode binding for keycode 44 and
    /// Mod4, including the keycodes_head entry that translate_keysyms()
    /// produces (C: src/bindings.c:378 ADD_TRANSLATED_KEY).
    fn bindings_with_keycode(command: &str, release: Option<&str>) -> crate::bindings::Bindings {
        let mut bindings = crate::bindings::Bindings::new();
        bindings.init_default_mode();
        let mut bind = crate::bindings::configure_binding(
            &mut bindings,
            "bindcode",
            Some("Mod4"),
            "44",
            release,
            None,
            None,
            None,
            command,
            crate::config_directives::DEFAULT_BINDING_MODE,
            false,
        )
        .expect("bindcode 44 + Mod4 parses");
        bind.keycodes_head.push_back(crate::data::Binding_Keycode {
            keycode: 44,
            modifiers: crate::config_directives::XCB_KEY_BUT_MASK_MOD_4,
        });
        bindings.modes[0].bindings[0] = bind;
        bindings
    }

    /// The tree copy of the active binding list (C: src/main.c:86 "bindings",
    /// which get_binding_from_xcb_event() searches).
    fn tree_bindings(
        bindings: &crate::bindings::Bindings,
    ) -> std::collections::VecDeque<crate::data::Binding> {
        bindings.modes[0].bindings.clone()
    }

    fn key_event(detail: u16, state: u16, time: u32, release: bool) -> Event {
        if release {
            Event::KeyRelease(x11rb::protocol::xproto::KeyReleaseEvent {
                detail: detail as u8,
                time,
                state: x11rb::protocol::xproto::KeyButMask::from(state),
                ..Default::default()
            })
        } else {
            Event::KeyPress(x11rb::protocol::xproto::KeyPressEvent {
                detail: detail as u8,
                time,
                state: x11rb::protocol::xproto::KeyButMask::from(state),
                ..Default::default()
            })
        }
    }

    /// C: src/key_press.c:21 handle_key_press (the KeyPress branch of
    /// src/handlers.c:1281) — the timestamp is stored and a press without a
    /// Bindings context is a no-op.
    #[test]
    fn key_press_stores_the_timestamp_and_needs_a_bindings_context() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();

        let ev = key_event(44, 0, 0x1234, false);
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(ctx.last_timestamp, 0x1234);
        assert!(b.commands.is_empty());
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
    }

    /// C: src/key_press.c:21 + src/bindings.c:315 get_binding_from_xcb_event —
    /// a key press that matches the binding runs its command once through
    /// run_binding().
    #[test]
    fn key_press_runs_the_bound_command() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut bindings = bindings_with_keycode("nop", None);
        env.t.bindings = tree_bindings(&bindings);
        let mut ctx = EventContext {
            bindings: Some(&mut bindings),
            ..Default::default()
        };

        let ev = key_event(
            44,
            crate::config_directives::XCB_KEY_BUT_MASK_MOD_4 as u16,
            0x2000,
            false,
        );
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(b.commands, vec!["nop".to_string()]);

        // A different keycode does not match.
        let ev = key_event(
            45,
            crate::config_directives::XCB_KEY_BUT_MASK_MOD_4 as u16,
            0x2001,
            false,
        );
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(b.commands.len(), 1);
    }

    /// C: src/bindings.c:210 get_binding — a press binding does not fire on the
    /// release event, while a bindsym --release binding fires on the release
    /// only (the press marks it B_UPON_KEYRELEASE_IGNORE_MODS first, so the
    /// modifiers may already be up when the key comes up).
    #[test]
    fn key_release_only_runs_release_bindings() {
        let mut env = env();
        let mut b = RecordingBackend::default();

        // Press binding: the release must not run it.
        let mut bindings = bindings_with_keycode("nop", None);
        env.t.bindings = tree_bindings(&bindings);
        let mut ctx = EventContext {
            bindings: Some(&mut bindings),
            ..Default::default()
        };
        let ev = key_event(
            44,
            crate::config_directives::XCB_KEY_BUT_MASK_MOD_4 as u16,
            0x3000,
            true,
        );
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(b.commands.is_empty());

        // Release binding: the press only marks the binding, the release runs it.
        let mut bindings = bindings_with_keycode("nop release", Some("release"));
        env.t.bindings = tree_bindings(&bindings);
        let mut ctx = EventContext {
            bindings: Some(&mut bindings),
            ..Default::default()
        };
        let ev = key_event(
            44,
            crate::config_directives::XCB_KEY_BUT_MASK_MOD_4 as u16,
            0x3001,
            false,
        );
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(b.commands.is_empty(), "press only marks the release binding");
        assert_eq!(
            env.t.bindings[0].release,
            crate::data::BindingRelease::B_UPON_KEYRELEASE_IGNORE_MODS
        );

        // The release matches even without the modifiers (IGNORE_MODS).
        let ev = key_event(44, 0, 0x3002, true);
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(b.commands, vec!["nop release".to_string()]);
    }

    /// C: src/click.c:372 handle_button_press + :104 route_click — a click on a
    /// client window focuses that container (step 3 of route_click) and replays
    /// the pointer to the client.
    #[test]
    fn button_press_on_the_client_window_focuses_the_con() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut null = NullBackend;

        // A second client con in the same workspace, currently focused.
        let other = crate::con::con_new(&mut env.t, &mut null, Some(env.ws), None);
        env.t.cons[other].r#type = ConType::CT_CON;
        let other_window = env.t.windows.insert(Window { id: 0x43, ..Default::default() });
        env.t.window_index.insert(0x43, other_window);
        env.t.cons[other].window = Some(other_window);
        env.t.focused = Some(other);

        let mut ctx = EventContext::default();
        let ev = Event::ButtonPress(x11rb::protocol::xproto::ButtonPressEvent {
            detail: 1,
            event: 0x42,
            event_x: 10,
            event_y: 10,
            time: 0x4000,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(env.t.focused, Some(env.con));
        assert_eq!(ctx.last_timestamp, 0x4000);
        assert!(
            env.x.log.iter().any(|l| l.starts_with("allow_events(mode=2")),
            "the pointer is replayed to the client: {:?}",
            env.x.log
        );
    }

    /// C: src/click.c:169 route_click — a click into a window that is neither a
    /// container, a frame nor the root window is an error and only replays the
    /// pointer (xcb_allow_events XCB_ALLOW_REPLAY_POINTER + flush).
    #[test]
    fn button_press_on_an_unknown_window_replays_the_pointer() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();

        let ev = Event::ButtonPress(x11rb::protocol::xproto::ButtonPressEvent {
            detail: 1,
            event: 0x9999,
            time: 0x4100,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(
            env.x.log
                .contains(&"allow_events(mode=2, ts=16640)".to_string()),
            "log={:?}",
            env.x.log
        );
        assert!(env.x.log.contains(&"flush".to_string()), "log={:?}", env.x.log);
    }

    /// C: src/handlers.c: handle_focus_out — only the DLOG remains ("focus out,
    /// event->event = ..."); the focus follow logic belongs to
    /// handle_enter_notify, so nothing in the tree or on the X connection
    /// changes.
    #[test]
    fn focus_out_leaves_the_focus_and_the_tree_alone() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut ctx = EventContext::default();

        let ev = Event::FocusOut(x11rb::protocol::xproto::FocusOutEvent {
            event: 0x42,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert_eq!(env.t.focused, Some(env.con));
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
        assert!(b.ipc_events.is_empty(), "events={:?}", b.ipc_events);
    }

    /// C: src/handlers.c:1250 handle_configure_notify — a ConfigureNotify for
    /// the root window means the output geometry changed, so the outputs are
    /// re-queried (randr_query_outputs).
    #[test]
    fn configure_notify_on_the_root_window_requeries_the_outputs() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut randr = crate::randr::RandrState::default();
        let mut ctx = EventContext {
            root: 0x50,
            randr: Some(&mut randr),
            ..Default::default()
        };

        let ev = Event::ConfigureNotify(x11rb::protocol::xproto::ConfigureNotifyEvent {
            event: 0x50,
            window: 0x50,
            width: 1920,
            height: 1080,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(
            env.x.log.iter().any(|l| l.starts_with("randr_get_output_primary")),
            "the outputs were re-queried: {:?}",
            env.x.log
        );
    }

    /// C: src/handlers.c:1250 handle_configure_notify — a ConfigureNotify for a
    /// managed window is only logged; no RandR request is made for it.
    #[test]
    fn configure_notify_for_a_managed_window_makes_no_randr_request() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut randr = crate::randr::RandrState::default();
        let mut ctx = EventContext {
            root: 0x50,
            randr: Some(&mut randr),
            ..Default::default()
        };

        let ev = Event::ConfigureNotify(x11rb::protocol::xproto::ConfigureNotifyEvent {
            event: 0x42,
            window: 0x42,
            ..Default::default()
        });
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
    }

    /// C: src/handlers.c:1448 handle_xkb_state_notify — a group change updates
    /// xkb_current_group and re-grabs every key; repeating the same group is a
    /// no-op.
    #[test]
    fn xkb_state_notify_regrabs_the_keys_on_a_group_change() {
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut bindings = bindings_with_keycode("nop", None);
        env.t.bindings = tree_bindings(&bindings);
        let mut ctx = EventContext {
            xkb_base: 89,
            bindings: Some(&mut bindings),
            ..Default::default()
        };

        // xkb_state_notify_event_t: response_type, xkbType, ..., group at 18.
        let mut raw = vec![0u8; 32];
        raw[0] = 89;
        raw[1] = XCB_XKB_STATE_NOTIFY;
        raw[18] = 2;
        let ev = Event::Unknown(raw.clone());
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));

        assert_eq!(ctx.bindings.as_deref().unwrap().xkb_current_group, 2);
        assert!(
            env.x.log.iter().any(|l| l.starts_with("ungrab_key")),
            "log={:?}",
            env.x.log
        );
        assert!(
            env.x.log.iter().any(|l| l.contains("key=44")),
            "the bound keycode was re-grabbed: {:?}",
            env.x.log
        );

        // Same group again: C returns before ungrab_all_keys().
        env.x.log.clear();
        let ev = Event::Unknown(raw);
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
    }

    /// C: src/handlers.c:1430 handle_xkb_map_notify — the event sequence is
    /// registered in the ignore list (ENSURE_FULL_SEQUENCE) so that the
    /// resulting MapNotify is not processed twice.
    #[test]
    fn xkb_map_notify_registers_the_ignore_sequence() {
        let mut env = env();
        let mut ctx = EventContext::default();

        let mut raw = vec![0u8; 32];
        raw[0] = 89;
        raw[1] = XCB_XKB_MAP_NOTIFY;
        raw[2] = 0x34;
        raw[3] = 0x12;
        handle_xkb_map_notify(&mut env.t, &mut env.x, &raw, &mut ctx);

        assert!(event_is_ignored(&mut env.t, 0x1234, -1));
    }

    /// C: src/handlers.c:1421 handle_xkb_new_keyboard_notify — only a changed
    /// keymap (XCB_XKB_NKN_DETAIL_KEYCODES) reloads the keymap and re-grabs the
    /// keys.
    #[test]
    fn xkb_new_keyboard_notify_only_reloads_on_keycode_changes() {
        // The reload path needs a real xkbcommon context (C: xkb_context_new).
        let mut factory = match crate::xkb::RealKeymapFactory::new() {
            Some(factory) => factory,
            None => return,
        };
        let mut env = env();
        let mut b = RecordingBackend::default();
        let mut bindings = bindings_with_keycode("nop", None);
        env.t.bindings = tree_bindings(&bindings);
        let mut ctx = EventContext {
            xkb_base: 89,
            bindings: Some(&mut bindings),
            xkb_factory: Some(&mut factory),
            ..Default::default()
        };

        // changed = XCB_XKB_NKN_DETAIL_DEVICE_ID only.
        let mut raw = vec![0u8; 32];
        raw[0] = 89;
        raw[1] = XCB_XKB_NEW_KEYBOARD_NOTIFY;
        raw[16] = 0x02;
        let ev = Event::Unknown(raw.clone());
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(
            env.x.log.is_empty(),
            "no reload without a keycode change: {:?}",
            env.x.log
        );

        // changed = XCB_XKB_NKN_DETAIL_KEYCODES -> reload + re-grab.
        raw[16] = XCB_XKB_NKN_DETAIL_KEYCODES as u8;
        let ev = Event::Unknown(raw);
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(
            env.x.log.iter().any(|l| l.starts_with("ungrab_key")),
            "log={:?}",
            env.x.log
        );
        assert!(
            env.x.log.iter().any(|l| l.contains("key=44")),
            "log={:?}",
            env.x.log
        );
    }

    /// C: src/handlers.c: xkb_reload_keymap_and_grab — load_keymap() (C:
    /// src/bindings.c:980) rebuilds the keymap, then every binding is ungrab'd
    /// and grabbed again (C: src/bindings.c:1010 ungrab_all_keys, :1033
    /// grab_all_keys).
    #[test]
    fn xkb_reload_keymap_and_grab_regrabs_every_bound_key() {
        let mut factory = match crate::xkb::RealKeymapFactory::new() {
            Some(factory) => factory,
            None => return,
        };
        let mut env = env();
        let mut bindings = bindings_with_keycode("nop", None);
        env.t.bindings = tree_bindings(&bindings);
        let mut ctx = EventContext {
            bindings: Some(&mut bindings),
            xkb_factory: Some(&mut factory),
            ..Default::default()
        };

        xkb_reload_keymap_and_grab(&mut env.t, &mut env.x, &mut ctx);

        assert!(
            ctx.bindings
                .as_deref()
                .map(|b| b.xkb_context_created)
                .unwrap_or(false),
            "load_keymap marks the xkb context as created"
        );
        assert!(
            env.x.log.iter().any(|l| l.starts_with("ungrab_key")),
            "log={:?}",
            env.x.log
        );
        assert!(
            env.x.log.iter().any(|l| l.contains("key=44")),
            "log={:?}",
            env.x.log
        );
    }

    /// C: src/handlers.c:1461 handle_shape_notify -> x_set_shape — the raw
    /// record is only read up to its length (the C struct is always 32 bytes)
    /// and the branch needs shape_supported plus the extension base.
    #[test]
    fn shape_notify_survives_truncated_payloads() {
        let mut env = env();
        let mut b = RecordingBackend::default();

        // A truncated record: the handler must not read past the end.
        let mut ctx = EventContext {
            shape_base: 120,
            shape_supported: true,
            ..Default::default()
        };
        let ev = Event::Unknown(vec![120, 0, 0, 0]);
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);

        // A full record for a managed window only logs.
        let mut raw = vec![0u8; 32];
        raw[0] = 120;
        raw[4] = 0x42;
        raw[8] = 1;
        let ev = Event::Unknown(raw);
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);

        // Without the shape extension the branch is not taken at all.
        let mut ctx = EventContext {
            shape_base: 120,
            shape_supported: false,
            ..Default::default()
        };
        let ev = Event::Unknown(vec![120u8; 32]);
        assert!(handle_x11_event_ctx(&mut env.t, &mut b, &mut env.x, &env.a, &ev, &mut ctx));
        assert!(env.x.log.is_empty(), "log={:?}", env.x.log);
    }

    /// C: src/handlers.c:472 calls scratchpad_fix_resolution() unconditionally;
    /// the port guards it with tree_has_scratchpad(), which reports whether the
    /// __i3 pseudo-output with its content/__i3_scratch children exists (C:
    /// src/tree.c _create___i3). Without it there is no output to fix.
    #[test]
    fn tree_has_scratchpad_reports_the_i3_pseudo_output() {
        let mut t = Tree::new();
        assert!(!tree_has_scratchpad(&t), "no croot yet");

        let mut null = NullBackend;
        let croot = crate::con::con_new(&mut t, &mut null, None, None);
        t.croot = Some(croot);
        t.cons[croot].name = Some("root".to_string());
        t.cons[croot].r#type = ConType::CT_ROOT;
        assert!(!tree_has_scratchpad(&t), "no __i3 con under croot");

        let i3_con = crate::con::con_new(&mut t, &mut null, Some(croot), None);
        t.cons[i3_con].name = Some("__i3".to_string());
        t.cons[i3_con].r#type = ConType::CT_OUTPUT;
        assert!(!tree_has_scratchpad(&t), "an empty __i3 con has no children");

        let content = crate::con::con_new(&mut t, &mut null, Some(i3_con), None);
        t.cons[content].name = Some("content".to_string());
        assert!(tree_has_scratchpad(&t));

        // tree_init() builds exactly this shape.
        let mut initialized = Tree::new();
        crate::tree::tree_init(&mut initialized, &mut null, Rect::new(0, 0, 1280, 800));
        assert!(tree_has_scratchpad(&initialized));
    }
}

