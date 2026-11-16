//! The real X11 implementation of ConBackend.
//!
//! C: the side effects that src/x.c, src/manage.c, src/tree.c, src/ewmh.c and
//! src/floating.c perform directly on the xcb connection. The tree layer calls
//! them through the ConBackend trait so that the same logic runs against a
//! RecordingConn / NullBackend in the unit tests.
//!
//! The X connection is shared, never re-opened: one RustXConn wraps the one
//! Rc<RustConnection> of the process (C has exactly one xcb_connection_t, and
//! xcb_grab_server would deadlock a second one). Creating a RustXConn per call
//! is a fresh handle onto that same connection, so request order is the call
//! order — same as C's global `conn`.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use crate::xcb_util::SharedConnection;

use libi3::draw_util::{
    color_t, draw_util_rectangle, draw_util_surface_init, draw_util_text, surface_t,
};

use crate::backend::{ConBackend, PropMode, XConn, XCB_CW_EVENT_MASK};
use crate::data::KillWindow;
use crate::data::{ConId, Rect, XCB_ATOM_NONE};
use crate::tree::Tree;
use crate::x::{ConDecoration, DecoColors, DrawBackend};
use crate::xcb_util::{Atoms, RustXConn};

/// C: xcb_window_t / XCB_NONE
const XCB_NONE: u32 = 0;
/// C: include/xcb.h XCB_SET_MODE_DELETE
const XCB_SET_MODE_DELETE: u8 = 1;
/// C: XCB_ATOM_CARDINAL (a predefined atom id)
const XCB_ATOM_CARDINAL: u32 = 6;
/// C: XCB_ICCCM_WM_STATE_WITHDRAWN
const XCB_ICCCM_WM_STATE_WITHDRAWN: u32 = 0;

/// The cairo/pango draw backend (C: the draw_util_*/draw_text calls that
/// src/x.c makes directly).
///
/// One surface_t per frame window, exactly like con->frame_buffer: the
/// surface is created with the frame's geometry and pushed to the frame
/// window in surface_commit().
pub struct CairoDraw {
    conn: SharedConnection,
    /// The depth of the frame windows (C: root_depth; a con with a custom
    /// depth uses its own, which the port does not model here).
    depth: u8,
    /// C: con->frame_buffer (cairo surface, gc and the pixmap it draws into),
    /// keyed by the frame window it belongs to.
    surfaces: HashMap<u32, FrameBuffer>,
    /// C: config.client.* — the color sets of the decorations.
    client: crate::config::config_client,
    /// C: config.font.height
    font_height: i32,
}

/// C: con->frame_buffer + the con->frame window it is copied to.
struct FrameBuffer {
    /// The cairo surface; `surface.id` is the pixmap (C: frame_buffer.id).
    surface: surface_t,
    /// C: con->frame.id — the window the pixmap is copied onto.
    window: u32,
    width: i32,
    height: i32,
}

impl CairoDraw {
    pub fn new(conn: SharedConnection, screen_idx: usize) -> CairoDraw {
        let depth = conn
            .setup()
            .roots
            .get(screen_idx)
            .map(|root| root.root_depth)
            .unwrap_or(24);
        CairoDraw {
            conn,
            depth,
            surfaces: HashMap::new(),
            client: crate::config::config_client::default(),
            font_height: 0,
        }
    }

    /// C: config.client / config.font — copied once after load_configuration().
    pub fn set_draw_config(&mut self, config: &crate::config::Configuration) {
        self.client = config.client;
        self.font_height = config.font.height;
    }

    /// Drops the surface and pixmap of a frame window that is going away
    /// (C: src/x.c:262-264 draw_util_surface_free + xcb_free_pixmap).
    pub fn drop_surface(&mut self, frame: u32) {
        if let Some(mut buffer) = self.surfaces.remove(&frame) {
            let conn = Rc::clone(&self.conn);
            libi3::draw_util::draw_util_surface_free(&*conn, &mut buffer.surface);
            let pixmap = buffer.surface.id;
            if pixmap != XCB_NONE {
                let _ = conn.free_pixmap(pixmap);
            }
        }
    }

    /// C: draw_util_rectangle() on the frame buffer.
    fn rect(&mut self, frame: u32, rect: Rect, color: color_t) {
        let conn = Rc::clone(&self.conn);
        if let Some(buffer) = self.surfaces.get_mut(&frame) {
            draw_util_rectangle(
                &*conn,
                &mut buffer.surface,
                color,
                rect.x as f64,
                rect.y as f64,
                rect.width as f64,
                rect.height as f64,
            );
        }
    }

    /// C: xcb_create_pixmap(conn, win_depth, con->frame_buffer.id,
    /// con->frame.id, width, height) + draw_util_surface_init().
    fn create_pixmap(&mut self, frame: u32, width: i32, height: i32) {
        let conn = Rc::clone(&self.conn);
        let depth = self.depth;
        let pixmap = match conn.generate_id() {
            Ok(id) => id,
            Err(_) => return,
        };
        if conn
            .create_pixmap(depth, pixmap, frame, width.max(1) as u16, height.max(1) as u16)
            .is_err()
        {
            return;
        }

        let mut surface = surface_t::none();
        draw_util_surface_init(&*conn, &mut surface, pixmap, depth, width, height);
        // C: draw_util_clear_surface(&(con->frame_buffer), black)
        libi3::draw_util::draw_util_clear_surface(
            &*conn,
            &mut surface,
            color_t { red: 0.0, green: 0.0, blue: 0.0, alpha: 1.0, colorpixel: 0 },
        );

        self.surfaces.insert(frame, FrameBuffer { surface, window: frame, width, height });
    }
}

impl DrawBackend for CairoDraw {
    fn draw_rect(&mut self, frame: u32, rect: Rect, color: color_t) {
        self.rect(frame, rect, color);
    }

    fn font_height(&self) -> i32 {
        self.font_height
    }

    fn deco_colors(&self, kind: ConDecoration) -> DecoColors {
        let triple = match kind {
            ConDecoration::Urgent => self.client.urgent,
            ConDecoration::Focused => self.client.focused,
            ConDecoration::FocusedTabTitle => self.client.focused_tab_title,
            ConDecoration::FocusedInactive => self.client.focused_inactive,
            ConDecoration::Unfocused => self.client.unfocused,
        };
        // C: p->background = config.client.background (a single color, not
        // part of the Colortriple).
        DecoColors {
            border: triple.border,
            background: triple.background,
            text: triple.text,
            indicator: triple.indicator,
            child_border: triple.child_border,
        }
    }

    fn logical_px(&self, logical: i32) -> i32 {
        libi3::dpi::logical_px(logical)
    }

    /// C: the frame_buffer pixmap creation of src/x.c:1048-1072.
    fn surface_set_size(&mut self, frame: u32, width: i32, height: i32) {
        if frame == XCB_NONE {
            return;
        }

        match self.surfaces.get(&frame) {
            Some(buffer) if buffer.window == frame && buffer.width == width && buffer.height == height => {
                // C: the pixmap is reused when neither the rect nor its size
                // changed.
                return;
            }
            Some(_) => {
                self.drop_surface(frame);
            }
            None => {}
        }
        self.create_pixmap(frame, width, height);
    }

    fn draw_clear(&mut self, frame: u32, color: color_t) {
        let conn = Rc::clone(&self.conn);
        if let Some(buffer) = self.surfaces.get_mut(&frame) {
            libi3::draw_util::draw_util_clear_surface(&*conn, &mut buffer.surface, color);
        }
    }

    /// C: src/x.c:360 x_draw_title_border
    fn draw_title_border(&mut self, frame: u32, deco: Rect, color: color_t) {
        let x = deco.x as f64;
        let y = deco.y as f64;
        let w = deco.width as f64;
        let h = deco.height as f64;
        // C: Left
        self.rect(frame, Rect::new(deco.x, deco.y, 1, deco.height), color);
        // C: Right
        self.rect(
            frame,
            Rect::new(deco.x + deco.width.saturating_sub(1), deco.y, 1, deco.height),
            color,
        );
        // C: Top
        self.rect(frame, Rect::new(deco.x, deco.y, deco.width, 1), color);
        // C: Bottom
        self.rect(
            frame,
            Rect::new(deco.x, deco.y + deco.height.saturating_sub(1), deco.width, 1),
            color,
        );
        let _ = (x, y, w, h);
    }

    fn draw_text_at(
        &mut self,
        frame: u32,
        x: i32,
        y: i32,
        max_width: i32,
        text: &str,
        fg: color_t,
        bg: color_t,
    ) {
        let conn = Rc::clone(&self.conn);
        if let Some(buffer) = self.surfaces.get_mut(&frame) {
            let mut string = libi3::i3string_from_utf8(text);
            draw_util_text(
                &*conn,
                &mut string,
                &mut buffer.surface,
                fg,
                bg,
                x,
                y,
                max_width,
            );
        }
    }

    /// C: src/x.c:769 draw_util_copy_surface(&(con->frame_buffer),
    /// &(con->frame), 0, 0, 0, 0, con->rect.width, con->rect.height)
    ///
    /// The cairo content and the XCB text are both already in the pixmap (every
    /// draw_util_* call flushes to it), so this is the X copy of the pixmap
    /// onto the frame window.
    fn surface_commit(&mut self, frame: u32) {
        let conn = Rc::clone(&self.conn);
        let Some(buffer) = self.surfaces.get(&frame) else {
            return;
        };
        if buffer.surface.id == XCB_NONE {
            return;
        }
        let _ = conn.copy_area(
            buffer.surface.id,
            buffer.window,
            buffer.surface.gc,
            0,
            0,
            0,
            0,
            buffer.width.max(1) as u16,
            buffer.height.max(1) as u16,
        );
    }
}

/// The real backend: the shared connection plus the frame state cache
/// (C: src/x.c state_for_frame) that x_push_changes needs.
pub struct XBackend {
    conn: SharedConnection,
    screen_idx: usize,
    atoms: Atoms,
    state: crate::x::XState,
    draw: CairoDraw,
    /// C: src/startup.c:24 startup_sequences + libsn (the startup notification
    /// state start_application() fills).
    startup: crate::startup::StartupState,
    /// C: the fork()/execl(_PATH_BSHELL) child block of src/startup.c:187-213.
    exec: Box<dyn crate::startup::ApplicationExec>,
    /// C: src/main.c:60 char *current_socketpath — start_application() exports
    /// it to the child as I3SOCK.
    socketpath: String,
    /// The `mode` commands issued by the command layer (C: cmd_mode calls
    /// switch_mode() directly); the event loop applies them.
    pending_mode_switches: RefCell<VecDeque<String>>,
}

impl XBackend {
    pub fn new(conn: SharedConnection, screen_idx: usize, atoms: Atoms) -> XBackend {
        let draw = CairoDraw::new(Rc::clone(&conn), screen_idx);
        XBackend {
            conn,
            screen_idx,
            atoms,
            state: crate::x::XState::new(),
            draw,
            startup: crate::startup::StartupState::new(),
            exec: Box::new(crate::startup::CommandExec),
            socketpath: String::new(),
            pending_mode_switches: RefCell::new(VecDeque::new()),
        }
    }

    /// C: the `config` global — colors and font metrics for the decorations.
    pub fn set_draw_config(&mut self, config: &crate::config::Configuration) {
        self.draw.set_draw_config(config);
    }

    /// C: src/main.c:60 char *current_socketpath — the path the IPC server
    /// bound in main() (config ipc-socket-path, else $I3SOCK, else the
    /// per-process file). start_application() exports it as I3SOCK.
    pub fn set_socketpath(&mut self, path: &str) {
        self.socketpath = path.to_string();
    }

    /// C: conn_screen (the startup notification screen index).
    pub fn screen(&self) -> i32 {
        self.screen_idx as i32
    }

    /// A fresh handle onto the one shared connection (C: the global `conn`).
    fn x(&self) -> RustXConn {
        RustXConn::wrap(Rc::clone(&self.conn), self.screen_idx)
    }
}

impl ConBackend for XBackend {
    /// C: src/commands.c:1160 cmd_move_window_to_mouse + xcb_query_pointer
    /// (src/floating.c:537 floating_move_to_pointer uses the same query).
    fn x_query_pointer_root(&mut self) -> Option<(u32, i32, i32)> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::ConnectionExt as _;
        let root = self.conn.setup().roots.get(self.screen_idx)?.root;
        let reply = self.conn.query_pointer(root).ok()?.reply().ok()?;
        Some((reply.child, i32::from(reply.root_x), i32::from(reply.root_y)))
    }
    /// C: src/bindings.c:879 parse_command(bind->command, NULL) — the command
    /// string of a key binding runs through the command parser. The default
    /// method of ConBackend is a no-op, so without this override every
    /// `bindsym … <command>` would match and then do nothing.
    fn parse_command(&mut self, t: &mut Tree, command: &str) -> bool {
        let mut runner = crate::commands::CommandRunner::new(t, self, None, None);
        let result = runner.run(command);
        result.needs_tree_render
    }

    /// C: src/startup.c:136 start_application — the fork()/setsid()/execl()
    /// child block runs through startup::start_application() with the real
    /// startup-notification state, the X connection (root cursor) and the
    /// /bin/sh -c spawn of startup::CommandExec.
    fn start_application(
        &mut self,
        t: &Tree,
        command: &str,
        no_startup_id: bool,
        window_id: u32,
    ) {
        let socketpath = self.socketpath.clone();
        let screen = self.screen();
        let mut x = self.x();
        crate::startup::start_application(
            &mut self.startup,
            &mut x,
            &mut *self.exec,
            t,
            t.focused,
            command,
            no_startup_id,
            window_id,
            screen,
            // C: last_timestamp (the global handlers.c keeps up to date)
            crate::x::get_last_timestamp(),
            &socketpath,
        );
    }

    /// C: src/commands.c:986 cmd_mode → switch_mode(); see
    /// ConBackend::switch_mode for why the request is queued.
    fn switch_mode(&mut self, _t: &mut Tree, mode: &str) {
        self.pending_mode_switches
            .borrow_mut()
            .push_back(mode.to_string());
    }

    fn take_mode_switch_request(&mut self) -> Option<String> {
        self.pending_mode_switches.borrow_mut().pop_front()
    }

    /// C: src/x.c:129 x_con_init — create the frame window for the con.
    ///
    /// Without this the Con has no frame: manage_window() would reparent the
    /// client into XCB_WINDOW_NONE and x_push_changes() would have nothing to
    /// map, so a managed client would never become visible.
    fn x_con_init(&mut self, t: &mut Tree, con: ConId) {
        let mut x = self.x();
        crate::x::x_con_init(
            t,
            &mut crate::backend::NullBackend,
            &mut x,
            &mut self.state,
            &mut self.draw,
            con,
        );
    }

    /// C: src/x.c:285 x_con_kill
    fn x_con_kill(&mut self, t: &mut Tree, con: ConId) {
        let frame = t.cons[con].frame.map(|f| f.id);
        let mut x = self.x();
        crate::x::x_con_kill(t, &mut crate::backend::NullBackend, &mut x, &mut self.state, con);
        // C: src/x.c:262-264 frees the frame_buffer surface together with the
        // frame. The port keeps the surfaces here, so drop it explicitly.
        if let Some(frame) = frame {
            self.draw.drop_surface(frame);
        }
    }

    /// C: src/x.c:1506 x_set_name — C only caches the name in con_state;
    /// x_push_node sets the WM_NAME property on the frame.
    fn x_set_name(&mut self, t: &Tree, con: ConId, name: &str) {
        if let Some(frame) = t.cons[con].frame {
            self.state.state_for_frame(frame.id).name = Some(name.to_string());
        }
    }

    /// C: src/x.c:1390 x_window_kill
    fn x_window_kill(&mut self, _t: &Tree, window_id: u32, kill_window: KillWindow) {
        let mut x = self.x();
        crate::x::x_window_kill(&mut x, &self.atoms, window_id, kill_window);
    }

    /// C: src/tree.c:226-236 — clear the event mask, unmap, reparent to root.
    fn x_reparent_window_to_root(&mut self, _t: &Tree, window_id: u32, x_pos: u32, y_pos: u32) {
        let mut x = self.x();
        x.change_window_attributes(window_id, &[(XCB_CW_EVENT_MASK, XCB_ATOM_NONE)]);
        x.unmap_window(window_id);
        let root = x.root_screen().root;
        x.reparent_window(window_id, root, x_pos as i16, y_pos as i16);
        // C: add_ignore_event(sequence, 0) — XConn hides the request sequence,
        // so an error for an already-destroyed window is not suppressed here.
    }

    /// C: src/tree.c:245-248 — WM_STATE = WITHDRAWN
    fn x_set_wm_state_withdrawn(&mut self, _t: &Tree, window_id: u32) {
        let mut x = self.x();
        let mut data = [0u8; 8];
        data[..4].copy_from_slice(&XCB_ICCCM_WM_STATE_WITHDRAWN.to_ne_bytes());
        x.change_property(
            window_id,
            self.atoms.A_WM_STATE,
            self.atoms.A_WM_STATE,
            32,
            PropMode::REPLACE,
            &data,
        );
    }

    /// C: src/tree.c:273 — xcb_change_save_set(XCB_SET_MODE_DELETE, ...)
    fn x_change_save_set(&mut self, _t: &Tree, window_id: u32) {
        let mut x = self.x();
        x.change_save_set(XCB_SET_MODE_DELETE, window_id);
    }

    /// C: src/tree.c:277-279 — stop ShapeNotify events.
    fn x_shape_select_input(&mut self, _t: &Tree, window_id: u32, enable: bool) {
        let mut x = self.x();
        x.shape_select_input(window_id, enable);
    }

    /// C: src/x.c:199 x_reinit — reset the cached frame state (child_mapped,
    /// window_rect, con) after the con's client window changed.
    fn x_reinit(&mut self, t: &mut Tree, con: ConId) {
        let mut x = self.x();
        crate::x::x_reinit(t, &mut x, &mut self.state, con);
    }

    /// C: src/x.c:1281 x_push_changes
    fn x_push_changes(&mut self, t: &mut Tree, con: ConId) {
        let mut x = self.x();
        crate::x::x_push_changes(
            t,
            &mut crate::backend::NullBackend,
            &mut x,
            &self.atoms,
            &mut self.state,
            &mut self.draw,
            con,
        );
    }

    /// C: src/x.c:1494 x_raise_con
    fn x_raise_con(&mut self, t: &Tree, con: ConId) {
        let mut x = self.x();
        crate::x::x_raise_con(t, &mut x, con);
    }

    /// C: src/x.c:1565 x_mask_event_mask
    fn x_mask_event_mask(&mut self, t: &Tree, mask: u32) {
        let mut x = self.x();
        crate::x::x_mask_event_mask(t, &mut x, &self.state, mask);
    }

    /// C: src/x.c:1553 x_set_warp_to — the mouse_warping check lives in x.c.
    fn x_set_warp_to(&mut self, t: &Tree, con: Option<ConId>) {
        crate::x::x_set_warp_to(t.config.mouse_warping, con);
    }

    /// C: src/x.c:234 x_move_win
    fn x_move_win(&mut self, t: &mut Tree, src: ConId, dst: ConId) {
        let mut x = self.x();
        crate::x::x_move_win(t, &mut x, src, dst);
    }

    /// C: src/x.c:219 x_reparent_child
    fn x_reparent_child(&mut self, t: &mut Tree, dst: ConId, src: ConId) {
        let mut x = self.x();
        crate::x::x_reparent_child(t, &mut x, dst, src);
    }

    /// C: src/ewmh.c:ewmh_update_wm_desktop
    fn ewmh_update_wm_desktop(&mut self, t: &mut Tree) {
        let mut x = self.x();
        crate::ewmh::ewmh_update_wm_desktop(t, &mut x, &self.atoms);
    }

    /// C: src/ewmh.c:ewmh_update_current_desktop
    fn ewmh_update_current_desktop(&mut self, t: &mut Tree) {
        let mut x = self.x();
        crate::ewmh::ewmh_update_current_desktop(t, &mut x, &self.atoms);
    }

    /// C: src/ewmh.c:ewmh_update_desktop_properties
    fn ewmh_update_desktop_properties(&mut self, t: &mut Tree) {
        let mut x = self.x();
        crate::ewmh::ewmh_update_desktop_properties(t, &mut x, &self.atoms);
    }

    /// C: src/ewmh.c:281 ewmh_update_sticky
    fn ewmh_update_sticky(&mut self, window_id: u32, sticky: bool) {
        let mut x = self.x();
        crate::ewmh::ewmh_update_sticky(&mut x, &self.atoms, window_id, sticky);
    }

    /// C: src/ewmh.c:218 ewmh_update_visible_name
    fn ewmh_update_visible_name(&mut self, window_id: u32, name: Option<&str>) {
        let mut x = self.x();
        crate::ewmh::ewmh_update_visible_name(&mut x, &self.atoms, window_id, name);
    }

    /// C: src/floating.c:42 floating_set_hint_atom
    fn floating_set_hint_atom(&mut self, t: &Tree, con: ConId, floating: bool) {
        fn recurse(b: &mut XBackend, t: &Tree, con: ConId, floating: bool) {
            let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
            for child in children {
                recurse(b, t, child, floating);
            }
            let window = match t.cons[con].window {
                Some(w) => t.windows[w].id,
                None => return,
            };
            let mut x = b.x();
            if floating {
                x.change_property(
                    window,
                    b.atoms.A_I3_FLOATING_WINDOW,
                    XCB_ATOM_CARDINAL,
                    32,
                    PropMode::REPLACE,
                    &1u32.to_ne_bytes(),
                );
            } else {
                x.delete_property(window, b.atoms.A_I3_FLOATING_WINDOW);
            }
        }
        recurse(self, t, con, floating);
    }

    /// C: src/x.c — add/remove _NET_WM_STATE_FULLSCREEN on the client window.
    fn x_set_net_wm_state_fullscreen(&mut self, _t: &Tree, window_id: u32, add: bool) {
        let mut x = self.x();
        if add {
            crate::xcb_util::xcb_add_property_atom(
                &mut x,
                window_id,
                self.atoms.A__NET_WM_STATE,
                self.atoms.A__NET_WM_STATE_FULLSCREEN,
            );
        } else {
            crate::xcb_util::xcb_remove_property_atom(
                &mut x,
                window_id,
                self.atoms.A__NET_WM_STATE,
                self.atoms.A__NET_WM_STATE_FULLSCREEN,
            );
        }
    }

    /// C: src/x.c:939 x_push_node (used by floating.c)
    fn x_push_node(&mut self, t: &mut Tree, con: ConId) {
        let mut x = self.x();
        crate::x::x_push_node(
            t,
            &mut crate::backend::NullBackend,
            &mut x,
            &self.atoms,
            &mut self.state,
            con,
        );
    }

    /// C: src/ipc.c:1642 ipc_send_window_event
    ///
    /// The payload is marshalled here and queued; the event loop forwards it
    /// to the IpcServer it owns (see crate::ipc::queue_window_event).
    fn ipc_send_window_event(&mut self, t: &Tree, event: &str, con: ConId) {
        crate::ipc::queue_window_event(t, event, con);
    }

    /// C: src/ipc.c:1626 ipc_send_workspace_event(change, current, old)
    ///
    /// `ws` is the event's "current" container and `current` the previously
    /// focused workspace, which C passes as `old`.
    fn ipc_send_workspace_event(&mut self, t: &Tree, event: &str, ws: ConId, current: Option<ConId>) {
        crate::ipc::queue_workspace_event(t, event, Some(ws), current);
    }

    /// C: no counterpart — src/x.c draws with the config globals. The draw
    /// backend holds a copy (set_draw_config), so a reload has to refresh it
    /// or the decorations would keep the previous colors and font height.
    fn apply_draw_config(&mut self, config: &crate::config::Configuration) {
        self.draw.set_draw_config(config);
    }

    /// C: src/ipc.c:1673 ipc_send_barconfig_update_event
    ///
    /// Queued like the window/workspace events (the IpcServer belongs to the
    /// event loop); commands that touch a bar's config (cmd_bar_mode,
    /// cmd_bar_hidden_state) and cmd_reload reach this through the backend.
    fn ipc_send_barconfig_update_event(&mut self, t: &Tree, barconfig: &crate::config::Barconfig) {
        crate::ipc::queue_barconfig_update_event(t, barconfig);
    }

    /// C: src/x.c:ewmh_update_active_window (kept for later phases)
    fn ewmh_update_misc(&mut self, _t: &Tree) {}
}
