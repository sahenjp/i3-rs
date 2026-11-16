//! x.c: The X11 frame/decoration/push layer (1:1 port of the tree-flow parts).
//!
//! C: src/x.c. The cairo/pango drawing is abstracted behind the DrawBackend
//! trait (NullDraw for tests; the cairo FFI lands in Phase 7). The X side
//! effects (reconfigure/stacking) are expressed through the XConn
//! abstraction so that x_push_changes' stack diff can be tested with a
//! RecordingConn.

use crate::backend::{
    ConfigWindow, ConBackend, PropMode, StackMode, XConn, XCB_CW_BACK_PIXEL, XCB_CW_BORDER_PIXEL,
    XCB_CW_COLORMAP, XCB_CW_EVENT_MASK, XCB_CW_OVERRIDE_REDIRECT,
};
use crate::con::con_is_leaf;
use crate::data::XCB_WINDOW_NONE;
use crate::data::{
    BorderStyle, ConId, ConType, Layout, Rect, deco_render_params, width_height, FrameSurface,
    KillWindow,
};
use crate::tree::Tree;
use crate::xcb_util::Atoms;

use std::sync::{Mutex, OnceLock};

thread_local! {
    /// C: src/x.c:20 'xcb_window_t focused_id = XCB_NONE;' — the window the X
    /// input focus was last set to.
    ///
    /// The C global is process-wide because the WM is one thread. The port
    /// keeps it per thread so that the unit tests (which run in parallel,
    /// each with their own Tree) cannot see each other's WM state; the real
    /// event loop is a single thread, so the behaviour is identical.
    static FOCUSED_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(XCB_WINDOW_NONE) };

    /// C: src/x.c:25 'static xcb_window_t last_focused = XCB_NONE;'
    static LAST_FOCUSED: std::cell::Cell<u32> = const { std::cell::Cell::new(XCB_WINDOW_NONE) };

    /// C: src/main.c:60 'xcb_timestamp_t last_timestamp = XCB_CURRENT_TIME;'
    ///
    /// The X layer reads this global for send_take_focus() and
    /// xcb_set_input_focus() (src/x.c:1415/1431/1452). The event loop mirrors
    /// its own last_timestamp here after every event, exactly like handlers.c
    /// assigns the C global (`last_timestamp = event->time`).
    static LAST_TIMESTAMP: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// C: reads the global focused_id.
pub fn focused_id() -> u32 {
    FOCUSED_ID.with(|v| v.get())
}

/// C: `focused_id = <window>`
pub fn set_focused_id(window: u32) {
    FOCUSED_ID.with(|v| v.set(window));
}

/// C: reads the static last_focused.
fn last_focused() -> u32 {
    LAST_FOCUSED.with(|v| v.get())
}

/// C: `last_focused = <window>` (always assigned together with focused_id).
fn set_last_focused(window: u32) {
    LAST_FOCUSED.with(|v| v.set(window));
}

/// C: `last_timestamp = event->time`
pub fn set_last_timestamp(timestamp: u32) {
    LAST_TIMESTAMP.with(|v| v.set(timestamp));
}

/// Reads the C global last_timestamp.
pub fn get_last_timestamp() -> u32 {
    LAST_TIMESTAMP.with(|v| v.get())
}

/// C: src/x.c:28 'static Rect *warp_to;'
///
/// C stores a *pointer* into the Con's rect and dereferences it in
/// x_push_changes() *after* x_push_node(), so the warp target is the rect as
/// updated by the render pass that runs before the push. Keeping the ConId
/// (instead of a copied Rect) reproduces that read-after-render timing.
static mut warp_to: Option<ConId> = None;

/// The per-frame state of the con (C: struct con_state in include/xcb.h —
/// the fields x_push_changes uses for the CIRCLEQ diff).
///
/// C: include/xcb.h (struct con_state)
#[derive(Debug, Clone, Default)]
pub struct con_state {
    /// the frame window id (xcb_window_t frame)
    pub id: u32,
    pub mapped: bool,
    pub unmap_count: u8,
    /// cached rect pushed to X11
    pub rect: Rect,
    /// whether this state was pushed at least once
    pub initialized: bool,
    /// C: src/x.c:38 'bool is_hidden;'
    pub is_hidden: bool,
    /// C: src/x.c:38 'bool is_maximized_vert;'
    pub is_maximized_vert: bool,
    /// C: src/x.c:38 'bool is_maximized_horz;'
    pub is_maximized_horz: bool,
    /// stacking order position from the last push (C: the CIRCLEQ position)
    pub old_stack_pos: i64,
    /// C: include/xcb.h con_state->name (pushed as WM_NAME on the frame)
    pub name: Option<String>,
    /// C: include/xcb.h con_state->child_mapped
    pub child_mapped: bool,
    /// C: include/xcb.h con_state->window_rect (last pushed client geometry)
    pub window_rect: Rect,
    /// C: include/xcb.h con_state->unmap_now
    pub unmap_now: bool,
    /// C: include/xcb.h con_state->con — the container this frame belongs to
    /// (ewmh_update_client_list / con_has_managed_window walk it).
    pub con: Option<ConId>,
}

/// The X state: con_states keyed by frame window id + the initial mapping
/// order (C: state_head CIRCLEQ order).
///
/// C: src/x.c:31 (state_head, initial_mapping_order)
#[derive(Debug, Default)]
pub struct XState {
    pub con_states: std::collections::HashMap<u32, con_state>,
    /// C: TAILQ_HEAD(initial_mapping_order, ...): con ids in tree order
    pub initial_mapping_order: std::collections::VecDeque<ConId>,
    /// C: src/x.c:31 'TAILQ_HEAD(initial_mapping_head, con_state)
    /// initial_mapping_order;' — the frame window ids in the order their
    /// con_state was created (TAILQ_INSERT_TAIL in x_con_init, TAILQ_REMOVE
    /// in _x_con_kill). ewmh_update_client_list() walks exactly this list.
    pub mapping_order: Vec<u32>,
    /// The last _NET_CLIENT_LIST pushed to X11.
    ///
    /// C: the update is gated on `stacking_changed`, which the CIRCLEQ diff
    /// computes. The port derives the stack order from the tree instead of
    /// keeping a second list of the pushed positions, so it gates on the
    /// resulting lists: the property is written exactly when its value would
    /// change (C may also rewrite an unchanged value for a re-initialized
    /// state, which is an invisible no-op on the X server).
    pub client_list: Vec<u32>,
    /// The last _NET_CLIENT_LIST_STACKING pushed to X11 (C: see above).
    pub client_list_stacking: Vec<u32>,
    /// Whether the client list hints were ever written.
    ///
    /// C: the first x_push_changes writes the hints even for zero clients
    /// because every fresh con_state has `initial` set (src/x.c:1335), so
    /// the root window carries an empty _NET_CLIENT_LIST from startup on.
    pub client_list_pushed: bool,
}

impl XState {
    pub fn new() -> XState {
        XState::default()
    }

    /// C: src/x.c:89 state_for_frame
    pub fn state_for_frame(&mut self, window: u32) -> &mut con_state {
        self.con_states.entry(window).or_insert_with(|| con_state {
            id: window,
            ..Default::default()
        })
    }
}

/// C: src/x.c:496-509 — which config.client color set a decoration uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConDecoration {
    Urgent,
    Focused,
    FocusedTabTitle,
    FocusedInactive,
    Unfocused,
}

/// C: struct Colortriple — the colors of one decoration state.
#[derive(Clone, Copy, Default)]
pub struct DecoColors {
    pub border: libi3::draw_util::color_t,
    pub background: libi3::draw_util::color_t,
    pub text: libi3::draw_util::color_t,
    pub indicator: libi3::draw_util::color_t,
    pub child_border: libi3::draw_util::color_t,
}

/// The draw backend: x_draw_decoration computes the rectangles and colors and
/// hands them to this trait; NullDraw is used in tests, CairoDraw in
/// crates/i3-core/src/xbackend.rs performs the cairo/pango drawing.
///
/// C: src/x.c (the cairo/pango drawing calls inside x_draw_decoration /
/// x_draw_title_border / draw_util_* of libi3).
pub trait DrawBackend {
    fn draw_rect(&mut self, _frame: u32, _rect: Rect, _color: libi3::draw_util::color_t) {}
    fn draw_text(&mut self, _frame: u32, _rect: Rect, _text: &str) {}

    /// C: config.font.height — used for the vertical centering of the title.
    fn font_height(&self) -> i32 {
        0
    }

    /// C: src/x.c:496-509 — pick p->color from the configuration.
    fn deco_colors(&self, _kind: ConDecoration) -> DecoColors {
        DecoColors::default()
    }

    /// C: libi3/dpi.c:85 logical_px (titles use logical_px(2) as padding).
    fn logical_px(&self, logical: i32) -> i32 {
        logical
    }

    /// C: draw_util_surface_init()/draw_util_surface_set_size() for the frame.
    fn surface_set_size(&mut self, _frame: u32, _width: i32, _height: i32) {}

    /// C: draw_util_clear_surface(&(con->frame_buffer), black)
    fn draw_clear(&mut self, _frame: u32, _color: libi3::draw_util::color_t) {}

    /// C: src/x.c:360 x_draw_title_border
    fn draw_title_border(&mut self, _frame: u32, _deco: Rect, _color: libi3::draw_util::color_t) {}

    /// C: draw_util_text(title, dest_surface, p->color->text,
    ///    p->color->background, x, y, max_width)
    fn draw_text_at(
        &mut self,
        _frame: u32,
        _x: i32,
        _y: i32,
        _max_width: i32,
        _text: &str,
        _fg: libi3::draw_util::color_t,
        _bg: libi3::draw_util::color_t,
    ) {
    }

    /// C: src/x.c:769 draw_util_copy_surface(&(con->frame_buffer), &(con->frame),
    ///    0, 0, 0, 0, con->rect.width, con->rect.height) — flush the finished
    ///    buffer to the X drawable.
    fn surface_commit(&mut self, _frame: u32) {}
}

/// NullDraw: records nothing, used by tests.
///
/// C: n/a (stands in for the absence of a renderer).
#[derive(Debug, Default)]
pub struct NullDraw;

impl DrawBackend for NullDraw {}

/// C: src/x.c:129 x_con_init
///
/// Creates the frame window for the con (32bpp visual/colormap when needed).
pub fn x_con_init(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, state: &mut XState, _draw: &mut dyn DrawBackend, con: ConId) {
    // C: depth = (con->depth == XCB_COPY_FROM_PARENT) ? root_depth : con->depth
    let root = x.root_screen();
    let depth = if t.cons[con].depth != 0 { t.cons[con].depth } else { root.root_depth };

    // C: visual = get_visualid_by_depth(depth); visual_type = get_visualtype_by_id(visual)
    let visual = crate::xcb_util::get_visualid_by_depth(x, depth);
    let _visual_type = crate::xcb_util::get_visualtype_by_id(x, visual);

    // C: a non-root depth needs its own colormap; the root depth uses the
    // default colormap.
    let win_colormap = if depth != root.root_depth {
        let colormap = x.generate_id();
        x.create_colormap(colormap, root.root, visual);
        colormap
    } else {
        root.default_colormap
    };

    // C: frame dimensions are (-15, -15) 10x10 and it is override-redirect:
    // "our own frames should not be managed". Without OVERRIDE_REDIRECT the
    // WM's own frame windows are viewable top-level windows and
    // manage_existing_windows()/MapRequest would manage them as clients.
    let frame = x.generate_id();
    let values = [
        (XCB_CW_BACK_PIXEL, root.black_pixel),
        (XCB_CW_BORDER_PIXEL, root.black_pixel),
        (XCB_CW_OVERRIDE_REDIRECT, 1),
        (
            XCB_CW_EVENT_MASK,
            FRAME_EVENT_MASK & !crate::backend::XCB_EVENT_MASK_ENTER_WINDOW,
        ),
        (XCB_CW_COLORMAP, win_colormap),
    ];
    x.create_window(
        depth,
        frame,
        root.root,
        -15,
        -15,
        10,
        10,
        0,
        crate::backend::XCB_WINDOW_CLASS_INPUT_OUTPUT,
        visual,
        &values,
    );

    // C: WM_CLASS = "i3-frame\0i3-frame\0" (helps debugging with xprop)
    x.change_property(
        frame,
        XCB_ATOM_WM_CLASS,
        XCB_ATOM_STRING,
        8,
        PropMode::REPLACE,
        b"i3-frame\0i3-frame\0",
    );

    // C: con->frame.id = frame; (the surface stub)
    t.con_mut(con).frame = Some(FrameSurface { id: frame });

    // C: src/x.c:182-189 — register the con_state in the frame state list and
    // in initial_mapping_order (TAILQ_INSERT_TAIL), which is the order
    // _NET_CLIENT_LIST is built from.
    state.state_for_frame(frame).con = Some(con);
    state.mapping_order.push(frame);

    // C: x_reinit(con) runs right after
    x_reinit(t, x, state, con);

    // C: the con name for debugging: "[i3 con] frame"
    b.x_con_init(t, con);
}

/// C: XCB_ATOM_WM_NAME / XCB_ATOM_STRING (predefined atoms, not interned).
const XCB_ATOM_WM_NAME: u32 = crate::data::XCB_ATOM_WM_NAME;
const XCB_ATOM_WM_CLASS: u32 = crate::data::XCB_ATOM_WM_CLASS;
const XCB_ATOM_STRING: u32 = 31;
/// C: XCB_ICCCM_WM_STATE_NORMAL (include/xcb.h)
const XCB_ICCCM_WM_STATE_NORMAL: u32 = 1;
/// C: XCB_ICCCM_WM_STATE_WITHDRAWN (include/xcb.h)
const XCB_ICCCM_WM_STATE_WITHDRAWN: u32 = 0;

/// C: include/xcb.h CHILD_EVENT_MASK — the mask set on client windows.
///
/// C: #define CHILD_EVENT_MASK (XCB_EVENT_MASK_PROPERTY_CHANGE |
///     XCB_EVENT_MASK_STRUCTURE_NOTIFY | XCB_EVENT_MASK_FOCUS_CHANGE)
/// — note the *absence* of XCB_EVENT_MASK_ENTER_WINDOW: the comment in x.c:1159
/// sets this mask "to be interested in EnterNotifys as soon as the window is
/// mapped" only for the frame, never for the client. Selecting EnterWindow on
/// the client makes i3 believe the user moved the pointer whenever a frame gets
/// mapped under it (handle_enter_notify then steals the focus).
pub const CHILD_EVENT_MASK: u32 = crate::backend::XCB_EVENT_MASK_PROPERTY_CHANGE
    | crate::backend::XCB_EVENT_MASK_STRUCTURE_NOTIFY
    | crate::backend::XCB_EVENT_MASK_FOCUS_CHANGE;

/// C: include/xcb.h FRAME_EVENT_MASK (the mask set on frame windows).
pub const FRAME_EVENT_MASK: u32 = crate::backend::XCB_EVENT_MASK_BUTTON_PRESS
    | crate::backend::XCB_EVENT_MASK_BUTTON_RELEASE
    | crate::backend::XCB_EVENT_MASK_POINTER_MOTION
    | crate::backend::XCB_EVENT_MASK_EXPOSURE
    | crate::backend::XCB_EVENT_MASK_STRUCTURE_NOTIFY
    | crate::backend::XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT
    | crate::backend::XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY
    | crate::backend::XCB_EVENT_MASK_ENTER_WINDOW;

/// C: include/xcb.h:42 ROOT_EVENT_MASK (the mask set on the root window).
pub const ROOT_EVENT_MASK: u32 = crate::backend::XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT
    | crate::backend::XCB_EVENT_MASK_BUTTON_PRESS
    /* when the user adds a screen (e.g. video projector), the root window
     * gets a ConfigureNotify */
    | crate::backend::XCB_EVENT_MASK_STRUCTURE_NOTIFY
    | crate::backend::XCB_EVENT_MASK_POINTER_MOTION
    | crate::backend::XCB_EVENT_MASK_PROPERTY_CHANGE
    | crate::backend::XCB_EVENT_MASK_FOCUS_CHANGE
    | crate::backend::XCB_EVENT_MASK_ENTER_WINDOW;

/// C: src/x.c:199 x_reinit
///
/// Reinitializes the associated X window state for this container after its
/// client window changed (reparenting when managing, swallowing a placeholder).
/// In particular child_mapped must be reset: x_push_node() only maps the
/// client window when !child_mapped, so a stale true leaves the new client
/// unmapped inside a mapped frame.
pub fn x_reinit(t: &mut Tree, x: &mut dyn XConn, state: &mut XState, con: ConId) {
    let _ = x;
    let frame = match t.cons[con].frame {
        Some(f) => f.id,
        None => {
            // C: ELOG("window state not found\n") — kept for 1:1 shape.
            crate::log::errorlog("window state not found\n");
            return;
        }
    };

    // C: src/x.c:207 DLOG("resetting state %p to initial\n", state);
    crate::log::dlog_at(
        file!(),
        "x_reinit",
        line!(),
        &format!("resetting state {:#010x} to initial\n", frame),
    );

    let s = state.state_for_frame(frame);
    // C: state->initial = true;
    //
    // The port uses "initialized" as the "geometry was pushed" cache, so the C
    // "initial" flag (which only makes the stack diff re-evaluate) has no
    // separate counterpart here — client_list_pushed covers that decision.
    // C: state->child_mapped = false;
    s.child_mapped = false;
    // C: state->con = con;
    s.con = Some(con);
    // C: memset(&(state->window_rect), 0, sizeof(Rect));
    s.window_rect = Rect::default();
}

/// C: src/x.c:219 x_reparent_child
///
/// Reparents the client window of src (old) into the frame of dst (con).
pub fn x_reparent_child(t: &mut Tree, x: &mut dyn XConn, dst: ConId, src: ConId) {
    // C: xcb_reparent_window(conn, window, con->frame.id, 0, 0)
    let window = t.cons[src].window.map(|w| t.windows[w].id).unwrap_or(XCB_WINDOW_NONE);
    let frame = t.cons[dst].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    let old_frame = t.cons[src].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    if window != XCB_WINDOW_NONE && frame != XCB_WINDOW_NONE {
        // C: src/x.c:980-995 — "Temporarily set the event masks to XCB_NONE so
        // that we won't get UnmapNotify events (otherwise the handler would
        // close the container). These events are generated automatically when
        // reparenting."
        if old_frame != XCB_WINDOW_NONE {
            x.change_window_attributes(old_frame, &[(XCB_CW_EVENT_MASK, crate::data::XCB_ATOM_NONE)]);
        }
        x.change_window_attributes(window, &[(XCB_CW_EVENT_MASK, crate::data::XCB_ATOM_NONE)]);
        x.reparent_window(window, frame, 0, 0);
        if old_frame != XCB_WINDOW_NONE {
            x.change_window_attributes(old_frame, &[(XCB_CW_EVENT_MASK, FRAME_EVENT_MASK)]);
        }
        x.change_window_attributes(window, &[(XCB_CW_EVENT_MASK, CHILD_EVENT_MASK)]);
        // C: con->ignore_unmap++; — the same reparent generates one UnmapNotify
        // for the *new* container, which must not close it.
        let con = t.con_mut(dst);
        con.ignore_unmap = con.ignore_unmap.saturating_add(1);
        crate::log::dlog_at(
            file!(),
            "x_reparent_child",
            line!(),
            &format!(
                "ignore_unmap for reparenting of con {} (win {:#010x}) is now {}\n",
                crate::manage::con_ptr(dst),
                window,
                t.cons[dst].ignore_unmap
            ),
        );
        x.map_window(window);
    }
}

/// C: src/x.c:234 x_move_win
///
/// Moves the client window from src con to dst con.
pub fn x_move_win(t: &mut Tree, x: &mut dyn XConn, src: ConId, dst: ConId) {
    // C: xcb_unmap_window(src window), reparent to dst frame
    let window = t.cons[src].window.map(|w| t.windows[w].id).unwrap_or(XCB_WINDOW_NONE);
    let frame = t.cons[dst].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    if window != XCB_WINDOW_NONE && frame != XCB_WINDOW_NONE {
        x.unmap_window(window);
        x.reparent_window(window, frame, 0, 0);
    }
    // C: move the window pointer between the cons
    let w = t.cons[src].window;
    t.con_mut(dst).window = w;
    t.con_mut(src).window = None;
}

/// C: src/x.c:256 _x_con_kill
fn _x_con_kill(t: &mut Tree, x: &mut dyn XConn, state: &mut XState, con: ConId) {
    // C: free the frame window + the pixmap state
    let frame = t.cons[con].frame.map(|f| f.id);
    if let Some(frame) = frame {
        state.con_states.remove(&frame);
        // C: src/x.c:268 TAILQ_REMOVE(&initial_mapping_head, state, ...)
        state.mapping_order.retain(|f| *f != frame);
        x.destroy_window(frame);
    }
    t.con_mut(con).frame = None;
}

/// C: src/x.c:285 x_con_kill
pub fn x_con_kill(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, state: &mut XState, con: ConId) {
    _x_con_kill(t, x, state, con);
    b.x_con_kill(t, con);
}

/// C: src/x.c:328 x_window_kill
pub fn x_window_kill(x: &mut dyn XConn, a: &crate::xcb_util::Atoms, window_id: u32, kill_window: KillWindow) {
    // C: if (!window_supports_protocol(window, A_WM_DELETE_WINDOW))
    let reply = x.get_property(window_id, a.A_WM_PROTOCOLS, 0 /* XCB_GET_PROPERTY_TYPE_ANY */, 0, 100);
    if !crate::xcb_util::xcb_reply_contains_atom(reply.as_ref(), a.A_WM_DELETE_WINDOW) {
        if kill_window == KillWindow::KILL_WINDOW {
            // C: LOG("Killing specific window 0x%08x") + xcb_destroy_window(conn, window)
            x.destroy_window(window_id);
        } else {
            // C: LOG("Killing the X11 client which owns window 0x%08x") + xcb_kill_client(conn, window)
            x.kill_client(window_id);
        }
        return;
    }

    // C: ev.response_type = XCB_CLIENT_MESSAGE; ev.type = A_WM_PROTOCOLS;
    //    ev.format = 32; ev.data.data32[0] = A_WM_DELETE_WINDOW;
    //    ev.data.data32[1] = XCB_CURRENT_TIME;
    x.send_client_message(window_id, false, 0, a.A_WM_PROTOCOLS, [
        a.A_WM_DELETE_WINDOW,
        0, /* XCB_CURRENT_TIME */
        0,
        0,
        0,
    ]);
    x.flush();
}

/// C: src/x.c:360 x_draw_title_border + 380 x_draw_decoration_after_title +
/// 459 x_draw_decoration: the rect/color computation is 1:1; the drawing goes
/// through DrawBackend.
///
/// deco_render_params cache: recomputed only when any of the cached params
/// changed (C: x_draw_decoration compares with con->deco_render_params).
pub fn x_draw_decoration(t: &mut Tree, draw: &mut dyn DrawBackend, con: ConId) {
    let parent = match t.cons[con].parent {
        Some(p) => p,
        None => return,
    };
    let leaf = con_is_leaf(t, con);

    // C: x_draw_decoration runs for leaf containers and for non-leaf
    // containers which are in a stacked/tabbed container. It does not run for
    // direct children of outputs or dockareas and not for floating containers.
    if (!leaf && t.cons[parent].layout != Layout::L_STACKED && t.cons[parent].layout != Layout::L_TABBED)
        || t.cons[parent].r#type == ConType::CT_OUTPUT
        || t.cons[parent].r#type == ConType::CT_DOCKAREA
        || t.cons[con].r#type == ConType::CT_FLOATING_CON
    {
        return;
    }

    // C: skip containers whose height is 0 (for example empty dockareas)
    if t.cons[con].rect.height == 0 {
        return;
    }

    // C: skip containers whose pixmap has not yet been created (can happen when
    // decoration rendering happens recursively for a window for which
    // x_push_node() was not yet called)
    if leaf && t.cons[con].frame.is_none() {
        return;
    }

    // C: struct deco_render_params new_params; ... compare with the cache
    let new_params = deco_render_params {
        color: None,
        border_style: match (t.cons[con].border_style, t.cons[con].current_border_width) {
            (BorderStyle::BS_NONE, _) => 0,
            (BorderStyle::BS_PIXEL, _) => 1,
            (BorderStyle::BS_NORMAL, _) => 2,
        },
        con_rect: width_height { w: t.cons[con].rect.width, h: t.cons[con].rect.height },
        con_window_rect: width_height {
            w: t.cons[con].window_rect.width,
            h: t.cons[con].window_rect.height,
        },
        con_deco_rect: t.cons[con].deco_rect,
        background: crate::data::color_t_stub::default(),
        parent_layout: t.cons[parent].layout,
        con_is_leaf: con_is_leaf(t, con),
    };

    // C: if (con->deco_render_params != NULL &&
    //        (con->window == NULL || !con->window->name_x_changed) &&
    //        !parent->pixmap_recreated && !con->pixmap_recreated &&
    //        !con->mark_changed &&
    //        memcmp(p, con->deco_render_params, sizeof(...)) == 0)
    //        -> skip the redraw
    // The port keeps the pixmap_recreated/mark_changed flags out of the con
    // (they belong to the render phase) and compares the struct contents
    // instead of memcmp; name_x_changed is part of the condition in C and is
    // honoured here as well.
    let name_x_changed = match t.cons[con].window {
        Some(w) => t.windows[w].name_x_changed,
        None => false,
    };
    let changed = name_x_changed || match &t.cons[con].deco_render_params {
        Some(cached) => {
            // C: the color field is a pointer into the config; the stub has no
            // contents yet (Phase 7 cairo), so only the presence is compared.
            cached.color.is_some() != new_params.color.is_some()
                || cached.border_style != new_params.border_style
                || cached.parent_layout != new_params.parent_layout
                || cached.con_is_leaf != new_params.con_is_leaf
                || cached.con_rect.w != new_params.con_rect.w
                || cached.con_rect.h != new_params.con_rect.h
                || cached.con_window_rect.w != new_params.con_window_rect.w
                || cached.con_window_rect.h != new_params.con_window_rect.h
                || cached.con_deco_rect != new_params.con_deco_rect
                || cached.background != new_params.background
        }
        None => true,
    };
    if !changed {
        return;
    }
    t.con_mut(con).deco_render_params = Some(Box::new(new_params));
    // C: src/x.c:540-542 — if (con->window != NULL &&
    //    con->window->name_x_changed) con->window->name_x_changed = false;
    if let Some(w) = t.cons[con].window {
        if t.windows[w].name_x_changed {
            t.windows[w].name_x_changed = false;
        }
    }

    // C: con->deco_rect is filled in by render_con() (src/render.c:453); the
    // port keeps a fallback for a tree that was pushed without a render pass.
    let rect = t.cons[con].rect;
    let deco = if t.cons[con].deco_rect.height == 0 {
        let deco = Rect::new(0, 0, rect.width, crate::render::render_deco_height(t) as u32);
        t.con_mut(con).deco_rect = deco;
        deco
    } else {
        t.cons[con].deco_rect
    };

    let frame = t.cons[con].frame.map(|f| f.id).unwrap_or(0);
    let window_rect = t.cons[con].window_rect;
    let colors = draw.deco_colors(deco_color_kind(t, con, leaf));
    let border_style = t.cons[con].border_style;

    // C: the frame surface covers the whole frame (src/x.c:1069 creates the
    // pixmap with con->rect dimensions).
    draw.surface_set_size(frame, rect.width as i32, rect.height as i32);

    // C: 2: draw the client.background, but only for the parts around the
    // window_rect
    if t.cons[con].window.is_some() {
        // C: Clear visible windows before beginning to draw
        draw.draw_clear(
            frame,
            libi3::draw_util::color_t { red: 0.0, green: 0.0, blue: 0.0, alpha: 1.0, colorpixel: 0 },
        );

        // C: top area
        draw.draw_rect(frame, Rect::new(0, 0, rect.width, window_rect.y), colors.background);
        // C: bottom area. The C coordinates are uint32_t and the additions in
        // x.c:465 wrap silently; wrapping_add keeps that behaviour instead of
        // panicking on overflow in a debug build.
        let bottom_y = window_rect.y.wrapping_add(window_rect.height);
        draw.draw_rect(
            frame,
            Rect::new(
                0,
                bottom_y,
                rect.width,
                rect.height.saturating_sub(bottom_y),
            ),
            colors.background,
        );
        // C: left area
        draw.draw_rect(frame, Rect::new(0, 0, window_rect.x, rect.height), colors.background);
        // C: right area (wrapping, see the bottom area above)
        let right_x = window_rect.x.wrapping_add(window_rect.width);
        draw.draw_rect(
            frame,
            Rect::new(
                right_x,
                0,
                rect.width.saturating_sub(right_x),
                rect.height,
            ),
            colors.background,
        );
    }

    // C: 3: draw a rectangle in border color around the client
    if border_style != BorderStyle::BS_NONE && leaf {
        let rectangles = x_get_border_rectangles(t, con);
        for r in &rectangles {
            draw.draw_rect(frame, *r, colors.child_border);
        }
    }

    // C: if this is a borderless/1pixel window, we don't need to render the
    // decoration.
    if border_style == BorderStyle::BS_NORMAL && deco.width != 0 && deco.height != 0 {
        // C: 4: paint the bar
        draw.draw_rect(frame, deco, colors.background);

        // C: 5: draw title border
        draw.draw_title_border(frame, deco, colors.border);

        // C: 6: draw the icon and title
        let text_offset_y = (deco.height as i32 - draw.font_height()) / 2;
        let title_padding = draw.logical_px(2);
        let deco_width = deco.width as i32;
        // C: src/x.c:683-698 — a window con uses win->name, a container its
        // title_format (not ported) or "i3: <tree representation>".
        let title = match t.cons[con].window {
            Some(w) => t.windows[w].name.clone().unwrap_or_default(),
            None => t.cons[con].name.clone().unwrap_or_default(),
        };
        // C: config.title_align == ALIGN_LEFT (the default):
        //    title_offset_x = title_padding + total_icon_space (no icons in
        //    the port), max_width = deco_width - mark_width - 2*title_padding.
        let title_offset_x = title_padding;
        draw.draw_text_at(
            frame,
            deco.x as i32 + title_offset_x,
            deco.y as i32 + text_offset_y,
            deco_width - 2 * title_padding,
            &title,
            colors.text,
            colors.background,
        );
    }

    // C: copy_pixmaps: draw_util_copy_surface(&(con->frame_buffer),
    //    &(con->frame), 0, 0, 0, 0, con->rect.width, con->rect.height);
    draw.surface_commit(frame);
}

/// C: src/x.c:496-509 — find out which colors to use for the decoration.
fn deco_color_kind(t: &Tree, con: ConId, leaf: bool) -> ConDecoration {
    let parent = match t.cons[con].parent {
        Some(parent) => parent,
        None => return ConDecoration::Unfocused,
    };

    if t.cons[con].urgent {
        ConDecoration::Urgent
    } else if t.focused == Some(con) || crate::con::con_inside_focused(t, con) {
        ConDecoration::Focused
    } else if t.cons[parent].focus_head.front() == Some(&con) {
        if leaf {
            ConDecoration::FocusedInactive
        } else {
            // C: config.client.got_focused_tab_title &&
            //    !leaf && con_descend_focused(con) == focused
            let focused = match t.focused {
                Some(focused) => focused,
                None => return ConDecoration::FocusedInactive,
            };
            if crate::con::con_descend_focused(t, con) == focused {
                ConDecoration::FocusedTabTitle
            } else {
                ConDecoration::FocusedInactive
            }
        }
    } else {
        ConDecoration::Unfocused
    }
}

/// C: src/x.c:409 x_get_border_rectangles
///
/// Returns the border rectangles (top/right/bottom/left) for the con.
pub fn x_get_border_rectangles(t: &Tree, con: ConId) -> Vec<Rect> {
    let mut rectangles: Vec<Rect> = Vec::new();
    let bs = t.cons[con].border_style;
    let cw = t.cons[con].current_border_width;
    if bs == BorderStyle::BS_NONE {
        return rectangles;
    }
    // C: (top)
    rectangles.push(Rect::new(0, 0, t.cons[con].rect.width, cw.max(0) as u32));
    // C: (right)
    rectangles.push(Rect::new(
        t.cons[con].rect.width.saturating_sub(cw.max(0) as u32),
        0,
        cw.max(0) as u32,
        t.cons[con].rect.height,
    ));
    // C: (bottom)
    let h = t.cons[con].rect.height;
    if bs == BorderStyle::BS_NORMAL {
        let deco_height = 18; // C: render_deco_height() stub
        rectangles.push(Rect::new(
            0,
            h.saturating_sub(deco_height as u32 + cw.max(0) as u32),
            t.cons[con].rect.width,
            cw.max(0) as u32,
        ));
    } else {
        rectangles.push(Rect::new(
            0,
            h.saturating_sub(cw.max(0) as u32),
            t.cons[con].rect.width,
            cw.max(0) as u32,
        ));
    }
    // C: (left)
    rectangles.push(Rect::new(0, 0, cw.max(0) as u32, t.cons[con].rect.height));
    rectangles
}

/// C: src/x.c:778 x_deco_recurse
pub fn x_deco_recurse(t: &mut Tree, draw: &mut dyn DrawBackend, con: ConId) {
    let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in children {
        x_deco_recurse(t, draw, child);
    }
    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for child in floating {
        x_deco_recurse(t, draw, child);
    }
    // C: if (child_is_leaf_or_container) x_draw_decoration(child)
    if t.cons[con].window.is_some() {
        x_draw_decoration(t, draw, con);
    }
}

/// C: src/x.c:808 set_hidden_state
///
/// Sets or removes the _NET_WM_STATE_HIDDEN property on con if necessary.
pub fn set_hidden_state(t: &mut Tree, x: &mut dyn XConn, a: &Atoms, state: &mut XState, con: ConId) {
    let window = match t.cons[con].window {
        Some(w) => w,
        None => return,
    };
    let frame = t.cons[con].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    if frame == XCB_WINDOW_NONE {
        return;
    }

    let window_id = t.windows[window].id;
    let should_be_hidden = crate::con::con_is_hidden(t, con);
    if should_be_hidden == state.state_for_frame(frame).is_hidden {
        return;
    }

    if should_be_hidden {
        crate::xcb_util::xcb_add_property_atom(x, window_id, a.A__NET_WM_STATE, a.A__NET_WM_STATE_HIDDEN);
    } else {
        crate::xcb_util::xcb_remove_property_atom(x, window_id, a.A__NET_WM_STATE, a.A__NET_WM_STATE_HIDDEN);
    }

    state.state_for_frame(frame).is_hidden = should_be_hidden;
}

/// C: src/x.c:834 set_maximized_state
///
/// Sets or removes _NET_WM_STATE_MAXIMIZE_{HORZ, VERT} on con.
pub fn set_maximized_state(t: &mut Tree, x: &mut dyn XConn, a: &Atoms, state: &mut XState, con: ConId) {
    let window = match t.cons[con].window {
        Some(w) => w,
        None => return,
    };
    let frame = t.cons[con].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    if frame == XCB_WINDOW_NONE {
        return;
    }
    let window_id = t.windows[window].id;

    let con_maximized_horz = crate::con::con_is_maximized(t, con, crate::data::Orientation::HORIZ);
    if con_maximized_horz != state.state_for_frame(frame).is_maximized_horz {
        if con_maximized_horz {
            crate::xcb_util::xcb_add_property_atom(x, window_id, a.A__NET_WM_STATE, a.A__NET_WM_STATE_MAXIMIZED_HORZ);
        } else {
            crate::xcb_util::xcb_remove_property_atom(x, window_id, a.A__NET_WM_STATE, a.A__NET_WM_STATE_MAXIMIZED_HORZ);
        }
        state.state_for_frame(frame).is_maximized_horz = con_maximized_horz;
    }

    let con_maximized_vert = crate::con::con_is_maximized(t, con, crate::data::Orientation::VERT);
    if con_maximized_vert != state.state_for_frame(frame).is_maximized_vert {
        if con_maximized_vert {
            crate::xcb_util::xcb_add_property_atom(x, window_id, a.A__NET_WM_STATE, a.A__NET_WM_STATE_MAXIMIZED_VERT);
        } else {
            crate::xcb_util::xcb_remove_property_atom(x, window_id, a.A__NET_WM_STATE, a.A__NET_WM_STATE_MAXIMIZED_VERT);
        }
        state.state_for_frame(frame).is_maximized_vert = con_maximized_vert;
    }
}

/// C: src/x.c:939 x_push_node — pushes the node's frame geometry and maps/unmaps.
pub fn x_push_node(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, a: &Atoms, state: &mut XState, con: ConId) {
    let frame = t.cons[con].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    if frame != XCB_WINDOW_NONE {
        // C: x.c:947-953 — push the cached frame name (x_set_name only caches).
        if let Some(name) = state.state_for_frame(frame).name.take() {
            x.change_property(
                frame,
                XCB_ATOM_WM_NAME,
                XCB_ATOM_STRING,
                8,
                PropMode::REPLACE,
                name.as_bytes(),
            );
        }

        // C: src/x.c:953-971 — a container without a client window is never
        // mapped (except a stacked/tabbed container whose decorations are
        // still visible). This is what makes x_push_changes() skip windowless
        // cons in the focus block (src/x.c:1405) and what unmaps the frame of
        // an empty workspace.
        let mut rect = t.cons[con].rect;
        if t.cons[con].window.is_none()
            && (t.cons[con].layout == Layout::L_STACKED || t.cons[con].layout == Layout::L_TABBED)
        {
            // C: the height of all decorations drawn on this frame
            let mut max_y = 0u32;
            let mut max_height = 0u32;
            for &child in &t.cons[con].nodes_head {
                let dr = t.cons[child].deco_rect;
                if dr.y >= max_y && dr.height >= max_height {
                    max_y = dr.y;
                    max_height = dr.height;
                }
            }
            rect.height = max_y + max_height;
            if rect.height == 0 {
                t.con_mut(con).mapped = false;
            }
        } else if t.cons[con].window.is_none() {
            // C: not a stacked or tabbed split container
            t.con_mut(con).mapped = false;
        }

        let r = rect;
        // C: only configure when the geometry changed (state cache)
        let pushed = state.state_for_frame(frame);
        if !pushed.initialized || pushed.rect != r {
            x.configure_window(
                frame,
                &[
                    (ConfigWindow::X, r.x as u32),
                    (ConfigWindow::Y, r.y as u32),
                    (ConfigWindow::WIDTH, r.width as u32),
                    (ConfigWindow::HEIGHT, r.height as u32),
                ],
            );
            let pushed = state.state_for_frame(frame);
            pushed.rect = r;
            pushed.initialized = true;
        }

        // C: x.c:1132-1140 — ditto for the child window.
        if let Some(win) = t.cons[con].window.map(|w| t.windows[w].id) {
            let window_rect = t.cons[con].window_rect;
            if state.state_for_frame(frame).window_rect != window_rect {
                crate::xcb_util::xcb_set_window_rect(x, win, window_rect);
                state.state_for_frame(frame).window_rect = window_rect;
            }
        }

        // C: x.c:1125-1141 — Set _NET_FRAME_EXTENTS according to the actual
        // decoration size. Clients (and the test suite) use it to know how
        // much of the frame is decoration around their window.
        if let Some(win) = t.cons[con].window.map(|w| t.windows[w].id) {
            let bsr = crate::con::con_border_style_rect(t, con);
            /* C: Rect r = { bsr.x, 0 - bsr.width - bsr.x, bsr.y,
             * 0 - bsr.height - bsr.y }; (int arithmetic) */
            let extents: [u32; 4] = [
                bsr.x,                                                       /* left */
                (0i32.wrapping_sub(bsr.width as i32).wrapping_sub(bsr.x as i32)) as u32, /* right */
                bsr.y,                                                       /* top */
                (0i32.wrapping_sub(bsr.height as i32).wrapping_sub(bsr.y as i32)) as u32, /* bottom */
            ];
            let mut data = [0u8; 16];
            for (i, value) in extents.iter().enumerate() {
                data[i * 4..i * 4 + 4].copy_from_slice(&value.to_ne_bytes());
            }
            x.change_property(
                win,
                a.A__NET_FRAME_EXTENTS,
                crate::ewmh::XCB_ATOM_CARDINAL,
                32,
                PropMode::REPLACE,
                &data,
            );
        }

        // C: x.c:1153-1198 — Map if the map state changed, and make sure the
        // child window is mapped when the container is mapped. Unmaps are
        // handled in x_push_node_unmaps().
        let con_mapped = t.cons[con].mapped;
        let (state_mapped, child_mapped) = {
            let s = state.state_for_frame(frame);
            (s.mapped, s.child_mapped)
        };
        let window = t.cons[con].window.map(|w| t.windows[w].id);
        if (state_mapped != con_mapped || (window.is_some() && !child_mapped)) && con_mapped {
            if let Some(win) = window {
                // C: WM_STATE = Normal — GTK apps need it for drag & drop.
                let mut data = [0u8; 8];
                data[..4].copy_from_slice(&XCB_ICCCM_WM_STATE_NORMAL.to_ne_bytes());
                x.change_property(win, a.A_WM_STATE, a.A_WM_STATE, 32, PropMode::REPLACE, &data);
            }

            if !child_mapped {
                if let Some(win) = window {
                    x.map_window(win);
                    // C: we are interested in EnterNotifys as soon as the
                    // window is mapped.
                    x.change_window_attributes(win, &[(XCB_CW_EVENT_MASK, CHILD_EVENT_MASK)]);
                    state.state_for_frame(frame).child_mapped = true;
                }
            }

            x.map_window(frame);
            x.change_window_attributes(frame, &[(XCB_CW_EVENT_MASK, FRAME_EVENT_MASK)]);
            x.flush();

            state.state_for_frame(frame).mapped = con_mapped;
        }
        state.state_for_frame(frame).unmap_now =
            (state.state_for_frame(frame).mapped != con_mapped) && !con_mapped;

        b.x_push_node(t, con);
    }

    set_hidden_state(t, x, a, state, con);
    set_maximized_state(t, x, a, state, con);

    // C: recurse in focus order to display the focused client in a stack first
    // when switching workspaces (reduces flickering).
    let children: Vec<ConId> = t.cons[con].focus_head.iter().copied().collect();
    for child in children {
        x_push_node(t, b, x, a, state, child);
    }
}

/// C: src/x.c:1255 is_con_attached
fn is_con_attached(t: &Tree, con: ConId) -> bool {
    t.cons[con].parent.is_some()
}

/// Changes the atoms on the root window and the windows themselves to properly
/// reflect the current focus for ewmh compliance.
///
/// C: src/x.c:108 change_ewmh_focus
fn change_ewmh_focus(x: &mut dyn XConn, a: &Atoms, new_focus: u32, old_focus: u32) {
    if new_focus == old_focus {
        return;
    }

    crate::ewmh::ewmh_update_active_window(x, a, new_focus);

    if new_focus != XCB_WINDOW_NONE {
        crate::ewmh::ewmh_update_focused(x, a, new_focus, true);
    }

    if old_focus != XCB_WINDOW_NONE {
        crate::ewmh::ewmh_update_focused(x, a, old_focus, false);
    }
}

/// Compute the desired stacking order of frame windows (bottom to top).
///
/// C: x_push_changes computes the order from the CIRCLEQ; the port derives it
/// by walking the tree: root content, then workspaces bottom-up, containers in
/// focus order, floating above tiling.
pub fn compute_stack_order(t: &Tree) -> Vec<u32> {
    stack_order_cons(t)
        .into_iter()
        .filter_map(|con| t.cons[con].frame.map(|f| f.id))
        .collect()
}

/// The cons in the bottom-to-top stacking order (C: the CIRCLEQ_FOREACH_REVERSE
/// walk over state_head that x_push_changes and ewmh_update_client_list use).
///
/// C: src/x.c:1300-1330 (the stack walk of x_push_changes)
pub fn stack_order_cons(t: &Tree) -> Vec<ConId> {
    let mut order: Vec<ConId> = Vec::new();
    fn visit(t: &Tree, con: ConId, order: &mut Vec<ConId>) {
        // C: the frames are stacked so that the focused container ends on top
        if t.cons[con].frame.is_some() {
            order.push(con);
        }
        // C: the dockarea cons are stacked before the content (they come first
        // in croot->nodes_head, so the plain tree walk already yields that
        // order). Floating windows follow their workspace's tiling children.
        for &child in &t.cons[con].nodes_head {
            visit(t, child, order);
        }
        for &child in &t.cons[con].floating_head {
            visit(t, child, order);
        }
    }
    if let Some(root) = t.croot {
        visit(t, root, &mut order);
    }
    order
}

/// C: src/x.c:1281 x_push_changes
///
/// Pushes the whole tree to X11: configure every changed frame and restack by
/// comparing the desired stack order with the previously pushed order,
/// emitting SIBLING/STACK_MODE configure requests for the diff only.
pub fn x_push_changes(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &Atoms,
    state: &mut XState,
    draw: &mut dyn DrawBackend,
    con: ConId,
) {
    // C: src/x.c:1285-1288 — "If we need to warp later, we request the pointer
    // position as soon as possible" (the reply is evaluated after x_push_node).
    let warp_target = unsafe { warp_to };
    let pointer_before = if warp_target.is_some() {
        let root = x.root_screen().root;
        x.query_pointer(root)
    } else {
        None
    };

    // C: src/x.c:1290 DLOG("-- PUSHING WINDOW STACK --\n");
    crate::log::dlog_at(file!(), "x_push_changes", line!(), "-- PUSHING WINDOW STACK --\n");

    // C: first push all nodes (geometry updates)
    x_push_node(t, b, x, a, state, con);

    // C: src/x.c:1376-1396 — warp the pointer to the container that is about to
    // be focused, but only when it is on a different output. Without this the
    // EnterNotify which X generates when a window on the *old* output is
    // unmapped under the cursor reaches handle_enter_notify(), and
    // check_crossing_screen_boundary() moves the focus back to the old output
    // (breaking e.g. t/516-move with fake outputs).
    if let Some(warp_target) = warp_target {
        // C: warp_to is a Rect* into the Con, dereferenced here (i.e. after
        // x_push_node and the render pass), so the rect is the up-to-date one.
        let warp_rect = t.cons.get(warp_target).map(|c| c.rect);
        match (pointer_before, warp_rect) {
            // C: ELOG("Could not query pointer position, not warping pointer\n")
            (None, _) => crate::log::errorlog("Could not query pointer position, not warping pointer\n"),
            (Some(_), None) => {}
            (Some(pointer), Some(warp_rect)) => {
                let mid_x = warp_rect.x.wrapping_add(warp_rect.width / 2);
                let mid_y = warp_rect.y.wrapping_add(warp_rect.height / 2);

                let current = crate::output::get_output_containing(t, pointer.root_x as u32, pointer.root_y as u32);
                let target = crate::output::get_output_containing(t, mid_x, mid_y);
                if current != target {
                    /* Ignore MotionNotify events generated by warping */
                    let root = x.root_screen().root;
                    x.change_window_attributes(root, &[(XCB_CW_EVENT_MASK, crate::backend::XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT)]);
                    x.warp_pointer(XCB_WINDOW_NONE, root, 0, 0, 0, 0, mid_x as i16, mid_y as i16);
                    x.change_window_attributes(root, &[(XCB_CW_EVENT_MASK, ROOT_EVENT_MASK)]);
                }
            }
        }

        /* C: warp_to = NULL; */
        unsafe {
            warp_to = None;
        }
    }

    // C: src/x.c:1294-1364 — the _NET_CLIENT_LIST / _NET_CLIENT_LIST_STACKING
    // hints. C builds them (and restacks) before x_push_node; the port keeps
    // its push order but the hints only depend on the tree, so the result is
    // the same.
    {
        // C: the bottom-to-top stack of all windows managed by i3.
        let mut stacking: Vec<u32> = Vec::new();
        for con in stack_order_cons(t) {
            if !crate::con::con_has_managed_window(t, con) {
                continue;
            }
            if let Some(w) = t.cons[con].window {
                stacking.push(t.windows[w].id);
            }
        }

        // C: reorder by initial mapping (TAILQ_FOREACH initial_mapping_head).
        let mut client_list: Vec<u32> = Vec::new();
        for frame in &state.mapping_order {
            let con = match state.con_states.get(frame).and_then(|s| s.con) {
                Some(con) => con,
                None => continue,
            };
            if !crate::con::con_has_managed_window(t, con) {
                continue;
            }
            if let Some(w) = t.cons[con].window {
                client_list.push(t.windows[w].id);
            }
        }

        // C: gated on `stacking_changed` (see XState::client_list), which is
        // true on the first push (state->initial) and whenever the hint's
        // value would change.
        if !state.client_list_pushed
            || stacking != state.client_list_stacking
            || client_list != state.client_list
        {
            crate::ewmh::ewmh_update_client_list_stacking(x, a, &stacking);
            crate::ewmh::ewmh_update_client_list(x, a, &client_list);
            state.client_list_stacking = stacking;
            state.client_list = client_list;
            state.client_list_pushed = true;
        }
    }

    // C: stack diff: walk the current order and restack changed positions
    let new_order = compute_stack_order(t);
    // C keeps the order in the con_state list, and a con_state is removed
    // from it when its Con is freed (src/x.c:268), so freed cons can never
    // appear here. The port stores ConIds, which may refer to a Con that was
    // freed since the last push; skip those instead of indexing them.
    let old_order: Vec<u32> = state
        .initial_mapping_order
        .iter()
        .filter_map(|c| t.cons.get(*c).and_then(|con| con.frame).map(|f| f.id))
        .collect();

    // C: x_restack: for each frame whose position changed, configure with
    // SIBLING (the window below it) + STACK_MODE (ABOVE).
    for (pos, &frame) in new_order.iter().enumerate() {
        let changed = old_order.get(pos) != Some(&frame);
        if !changed {
            continue;
        }
        // C: xcb_change_window_value_list: SIBLING = the previous frame,
        // STACK_MODE = ABOVE (or BELOW for the very first frame).
        let mut values: Vec<(ConfigWindow, u32)> = Vec::new();
        if pos > 0 {
            values.push((ConfigWindow::SIBLING, new_order[pos - 1]));
            values.push((ConfigWindow::STACK_MODE, StackMode::ABOVE as u32));
        } else {
            values.push((ConfigWindow::STACK_MODE, StackMode::BELOW as u32));
        }
        x.configure_window(frame, &values);
    }

    // remember the pushed order
    state.initial_mapping_order.clear();
    for con_id in all_cons_in_order(t) {
        state.initial_mapping_order.push_back(con_id);
    }

    // C: src/x.c:1397 x_deco_recurse(con) — draw every decoration onto the
    // frame buffers. Without this the frames stay black and no title is ever
    // rendered.
    x_deco_recurse(t, draw, con);

    // C: src/x.c:1400-1459 — the input focus. Without this the client never
    // gets the X input focus (typing goes nowhere) and _NET_ACTIVE_WINDOW
    // stays unset.
    push_input_focus(t, x, a);

    // C: focused_id == XCB_NONE (src/x.c:1447-1457).
    if focused_id() == XCB_WINDOW_NONE {
        // C: If we still have no window to focus, we focus the EWMH window
        // instead (avoids the ghosting effect of the root window, #1378).
        let ewmh_window = crate::ewmh::get_ewmh_window();
        // C: src/x.c:1450 DLOG("Still no window focused, better set focus to
        // the EWMH support window (%d)\n", ewmh_window);
        crate::log::dlog_at(
            file!(),
            "x_push_changes",
            line!(),
            &format!(
                "Still no window focused, better set focus to the EWMH support window ({})\n",
                ewmh_window
            ),
        );
        x.set_input_focus(ewmh_window, get_last_timestamp());
        change_ewmh_focus(x, a, XCB_WINDOW_NONE, last_focused());

        set_focused_id(ewmh_window);
        set_last_focused(XCB_WINDOW_NONE);
    }

    // C: that flush ends the focus phase.
    x.flush();
    // C: src/x.c:1457 DLOG("ENDING CHANGES\n");
    crate::log::dlog_at(file!(), "x_push_changes", line!(), "ENDING CHANGES\n");

    // C: src/x.c:1462-1475 — disable EnterWindow events for the windows which
    // are about to be unmapped, so that the EnterNotify of a window appearing
    // under the cursor cannot steal the focus.
    let enter_off = FRAME_EVENT_MASK & !crate::backend::XCB_EVENT_MASK_ENTER_WINDOW;
    let unmapping: Vec<u32> = state
        .con_states
        .iter()
        .filter(|(_, s)| s.unmap_now)
        .map(|(frame, _)| *frame)
        .collect();
    for frame in unmapping {
        x.change_window_attributes(frame, &[(XCB_CW_EVENT_MASK, enter_off)]);
    }

    // C: src/x.c:1478 — push all pending unmaps
    x_push_node_unmaps(t, x, a, state, con);

    x.flush();
}

/// The focus block of x_push_changes.
///
/// C: src/x.c:1404-1446
fn push_input_focus(t: &Tree, x: &mut dyn XConn, a: &Atoms) {
    let focused = match t.focused {
        Some(focused) => focused,
        None => return,
    };
    let frame = t.cons[focused].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    let window = t.cons[focused].window.map(|w| t.windows[w].id);
    let to_focus = match window {
        Some(window) => window,
        None => frame,
    };

    if focused_id() == to_focus {
        return;
    }

    if !t.cons[focused].mapped {
        // C: src/x.c:1405 DLOG("Not updating focus (to %p / %s), focused
        // window is not mapped.\n", focused, focused->name);
        crate::log::dlog_at(
            file!(),
            "push_input_focus",
            line!(),
            &format!(
                "Not updating focus (to {} / {}), focused window is not mapped.\n",
                crate::manage::con_ptr(focused),
                t.cons[focused].name.clone().unwrap_or_default()
            ),
        );
        // Invalidate focused_id to correctly focus new windows with the same ID.
        set_focused_id(XCB_WINDOW_NONE);
        return;
    }

    let last = last_focused();
    let (needs_take_focus, doesnt_accept_focus) = match t.cons[focused].window {
        Some(w) => (t.windows[w].needs_take_focus, t.windows[w].doesnt_accept_focus),
        None => (false, false),
    };
    // C: con_has_managed_window(focused) ? focused->window->id : XCB_WINDOW_NONE
    let managed_window = if crate::con::con_has_managed_window(t, focused) {
        window.unwrap_or(XCB_WINDOW_NONE)
    } else {
        XCB_WINDOW_NONE
    };
    let attached = is_con_attached(t, focused);

    if window.is_some() && needs_take_focus && doesnt_accept_focus {
        // C: src/x.c:1411 DLOG("Updating focus by sending WM_TAKE_FOCUS to
        // window 0x%08x (focused: %p / %s)\n", ...)
        crate::log::dlog_at(
            file!(),
            "push_input_focus",
            line!(),
            &format!(
                "Updating focus by sending WM_TAKE_FOCUS to window {:#010x} (focused: {} / {})\n",
                to_focus,
                crate::manage::con_ptr(focused),
                t.cons[focused].name.clone().unwrap_or_default()
            ),
        );
        crate::xcb_util::send_take_focus(x, a, to_focus, get_last_timestamp());

        change_ewmh_focus(x, a, managed_window, last);

        if to_focus != last && attached {
            crate::ipc::queue_window_event(t, "focus", focused);
        }
    } else {
        // C: src/x.c:1425 DLOG("Updating focus (focused: %p / %s) to X11
        // window 0x%08x\n", focused, focused->name, to_focus);
        crate::log::dlog_at(
            file!(),
            "push_input_focus",
            line!(),
            &format!(
                "Updating focus (focused: {} / {}) to X11 window {:#010x}\n",
                crate::manage::con_ptr(focused),
                t.cons[focused].name.clone().unwrap_or_default(),
                to_focus
            ),
        );
        // C: We remove XCB_EVENT_MASK_FOCUS_CHANGE from the event mask to get
        // no focus change events for our own focus changes. We only want
        // these generated by the clients.
        if let Some(window) = window {
            x.change_window_attributes(
                window,
                &[(
                    XCB_CW_EVENT_MASK,
                    CHILD_EVENT_MASK & !crate::backend::XCB_EVENT_MASK_FOCUS_CHANGE,
                )],
            );
        }
        x.set_input_focus(to_focus, get_last_timestamp());
        if let Some(window) = window {
            x.change_window_attributes(window, &[(XCB_CW_EVENT_MASK, CHILD_EVENT_MASK)]);
        }

        change_ewmh_focus(x, a, managed_window, last);

        if to_focus != XCB_WINDOW_NONE && to_focus != last && window.is_some() && attached {
            crate::ipc::queue_window_event(t, "focus", focused);
        }
    }

    set_focused_id(to_focus);
    set_last_focused(to_focus);
}

/// All cons in tree order (C: the CIRCLEQ order via con_walk).
fn all_cons_in_order(t: &Tree) -> Vec<ConId> {
    let mut result = Vec::new();
    fn visit(t: &Tree, con: ConId, out: &mut Vec<ConId>) {
        out.push(con);
        for &child in &t.cons[con].nodes_head {
            visit(t, child, out);
        }
        for &child in &t.cons[con].floating_head {
            visit(t, child, out);
        }
    }
    if let Some(root) = t.croot {
        visit(t, root, &mut result);
    }
    result
}

/// C: src/x.c:1213 x_push_node_unmaps
///
/// Unmap the frames whose map state changed (x_push_node set `unmap_now`).
/// This is split off from x_push_node so that a new fullscreen client can be
/// mapped and focused *before* the windows underneath it are unmapped.
pub fn x_push_node_unmaps(t: &mut Tree, x: &mut dyn XConn, a: &Atoms, state: &mut XState, con: ConId) {
    // C: map/unmap if the map state changed
    if let Some(frame) = t.cons[con].frame.map(|f| f.id) {
        if state.state_for_frame(frame).unmap_now {
            if let Some(w) = t.cons[con].window.map(|w| t.windows[w].id) {
                // C: Set WM_STATE_WITHDRAWN, it seems like Java apps need it
                let mut data = [0u8; 8];
                data[..4].copy_from_slice(&XCB_ICCCM_WM_STATE_WITHDRAWN.to_ne_bytes());
                x.change_property(w, a.A_WM_STATE, a.A_WM_STATE, 32, PropMode::REPLACE, &data);
            }

            x.unmap_window(frame);

            // C: we need to increase ignore_unmap for this container (if it
            // contains a window) and for every window "under" this one
            if t.cons[con].window.is_some() {
                t.con_mut(con).ignore_unmap = t.cons[con].ignore_unmap.saturating_add(1);
            }
            let con_mapped = t.cons[con].mapped;
            state.state_for_frame(frame).mapped = con_mapped;
        }
    }

    // C: handle all children and floating windows of this node
    let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in children {
        x_push_node_unmaps(t, x, a, state, child);
    }
    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for child in floating {
        x_push_node_unmaps(t, x, a, state, child);
    }
}

/// C: src/x.c:1494 x_raise_con
pub fn x_raise_con(t: &Tree, x: &mut dyn XConn, con: ConId) {
    // C: xcb_change_window_value_list(con->frame.id, STACK_MODE = ABOVE)
    let frame = t.cons[con].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    if frame != XCB_WINDOW_NONE {
        x.configure_window(frame, &[(ConfigWindow::STACK_MODE, StackMode::ABOVE as u32)]);
    }
}

/// C: src/x.c:1506 x_set_name
pub fn x_set_name(b: &mut dyn ConBackend, t: &Tree, con: ConId, name: &str) {
    b.x_set_name(t, con, name);
}

/// C: src/x.c:1535 x_set_i3_atoms
pub fn x_set_i3_atoms(x: &mut dyn XConn, a: &Atoms) {
    // C: src/x.c:1535 x_set_i3_atoms — the root window properties that other
    // i3 tools (i3-msg, i3-dump-log, i3-save-tree and the testsuite's
    // get_socket_path()) read to find this instance.
    let pid = std::process::id();
    let root = x.root_screen().root;
    let socketpath = i3_atom_socketpath();
    let configpath = i3_atom_configpath();
    // C: xcb_change_property(... A_I3_SOCKET_PATH, A_UTF8_STRING, 8,
    //    (current_socketpath == NULL ? 0 : strlen(current_socketpath)),
    //    current_socketpath);
    x.change_property(
        root,
        a.A_I3_SOCKET_PATH,
        a.A_UTF8_STRING,
        8,
        PropMode::REPLACE,
        socketpath.unwrap_or_default().as_bytes(),
    );
    // C: xcb_change_property(... A_I3_PID, XCB_ATOM_CARDINAL, 32, 1, &pid);
    x.change_property(
        root,
        a.A_I3_PID,
        6, /* XCB_ATOM_CARDINAL */
        32,
        PropMode::REPLACE,
        &pid.to_le_bytes(),
    );
    // C: xcb_change_property(... A_I3_CONFIG_PATH, A_UTF8_STRING, 8,
    //    strlen(current_configpath), current_configpath);
    x.change_property(
        root,
        a.A_I3_CONFIG_PATH,
        a.A_UTF8_STRING,
        8,
        PropMode::REPLACE,
        configpath.as_bytes(),
    );
    // C: xcb_change_property(... A_I3_LOG_STREAM_SOCKET_PATH, A_UTF8_STRING,
    //    8, strlen(current_log_stream_socket_path), current_log_stream_socket_path);
    //
    // C reads the global of src/log.c:390 here. It is set by create_socket()
    // in main.c:1016-1017, which runs before x_set_i3_atoms() (main.c:1069),
    // so the strlen() cannot see a NULL pointer there. The port deletes the
    // property instead when no path is known (the same shape
    // update_shmlog_atom() uses), which is what i3-dump-log -f reports as
    // "could not determine i3 log stream socket path".
    match crate::log::current_log_stream_socket_path() {
        Some(path) => x.change_property(
            root,
            a.A_I3_LOG_STREAM_SOCKET_PATH,
            a.A_UTF8_STRING,
            8,
            PropMode::REPLACE,
            path.as_bytes(),
        ),
        None => x.delete_property(root, a.A_I3_LOG_STREAM_SOCKET_PATH),
    }
    // C: src/x.c:1544 update_shmlog_atom();
    update_shmlog_atom(x, a);
    x.flush();
}

/// C: src/x.c:1521 update_shmlog_atom — while SHM logging is off the
/// I3_SHMLOG_PATH property is deleted, otherwise it carries the segment name
/// so that i3-dump-log can shm_open() the ring buffer.
///
/// Called from x_set_i3_atoms() and (like C: src/commands.c:2347) from
/// cmd_shmlog, which requests it through log::request_shmlog_atom_update()
/// because the command layer has no X connection.
pub fn update_shmlog_atom(x: &mut dyn XConn, a: &Atoms) {
    let root = x.root_screen().root;
    match crate::log::shmlog_name() {
        Some(name) => x.change_property(
            root,
            a.A_I3_SHMLOG_PATH,
            a.A_UTF8_STRING,
            8,
            PropMode::REPLACE,
            name.as_bytes(),
        ),
        None => x.delete_property(root, a.A_I3_SHMLOG_PATH),
    }
}

/// C: the process-wide globals that x_set_i3_atoms() exports. C keeps them in
/// src/ipc.c (current_socketpath) and src/config.c (current_configpath); the
/// port mirrors them here so the reload path (src/commands.c:1685) can re-export
/// them without threading two more parameters through reload().
/// current_log_stream_socket_path lives in src/log.c and is read from
/// crate::log directly (src/x.c:1544).
fn atom_paths() -> &'static Mutex<(Option<String>, String)> {
    static PATHS: OnceLock<Mutex<(Option<String>, String)>> = OnceLock::new();
    PATHS.get_or_init(|| Mutex::new((None, String::new())))
}

/// C: src/commands.c:1685 x_set_i3_atoms() after a reload — the reload only
/// rewrote current_configpath (src/config.c:235), current_socketpath and
/// current_log_stream_socket_path are left as they are, which is what keeps
/// i3-msg (it reads I3_SOCKET_PATH) working after "reload".
pub fn set_i3_atom_configpath(configpath: &str) {
    if let Ok(mut p) = atom_paths().lock() {
        p.1 = configpath.to_string();
    }
}

/// C: current_socketpath = ... (src/main.c, src/ipc.c)
pub fn set_i3_atom_paths(socketpath: Option<&str>, configpath: &str) {
    if let Ok(mut p) = atom_paths().lock() {
        *p = (
            socketpath.map(|s| s.to_string()),
            configpath.to_string(),
        );
    }
}

fn i3_atom_socketpath() -> Option<String> {
    atom_paths().lock().ok().and_then(|p| p.0.clone())
}

fn i3_atom_configpath() -> String {
    atom_paths().lock().map(|p| p.1.clone()).unwrap_or_default()
}


/// C: src/x.c:1553 x_set_warp_to
///
/// C only stores the pending warp when mouse_warping is enabled; with
/// "mouse_warping none" the pending target (if any) is left untouched.
pub fn x_set_warp_to(mouse_warping: crate::data::Warping, con: Option<ConId>) {
    if mouse_warping != crate::data::Warping::POINTER_WARPING_NONE {
        unsafe {
            warp_to = con;
        }
    }
}

/// C: src/x.c:1565 x_mask_event_mask
///
/// Sets `FRAME_EVENT_MASK & mask` as the event mask of every mapped frame.
/// The drag/resize loops (src/drag.c, src/resize.c, src/tiling_drag.c) use it
/// to drop EnterNotify while the user is dragging, so the focus does not move
/// under the pointer.
pub fn x_mask_event_mask(_t: &Tree, x: &mut dyn XConn, state: &XState, mask: u32) {
    let values = [(XCB_CW_EVENT_MASK, FRAME_EVENT_MASK & mask)];
    for (frame, con_state) in state.con_states.iter() {
        if con_state.mapped {
            x.change_window_attributes(*frame, &values);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::xcb_util::RecordingConn;

    fn build_tree_with_frames() -> (Tree, XState, RecordingConn, Vec<ConId>) {
        let mut t = Tree::new();
        crate::tree::tree_init(&mut t, &mut NullBackend, Rect::new(0, 0, 1280, 800));
        let mut state = XState::new();
        let mut x = RecordingConn::new(0x50);
        let mut draw = NullDraw;
        // root -> output -> content -> ws (con_new attaches to the parent
        // via con_attach, exactly like C: con.c:72)
        let root = t.croot.unwrap();
        let output = crate::con::con_new(&mut t, &mut NullBackend, Some(root), None);
        t.con_mut(output).r#type = ConType::CT_OUTPUT;
        x_con_init(&mut t, &mut NullBackend, &mut x, &mut state, &mut draw, output);
        let content = crate::con::con_new(&mut t, &mut NullBackend, Some(output), None);
        t.con_mut(content).r#type = ConType::CT_CON;
        x_con_init(&mut t, &mut NullBackend, &mut x, &mut state, &mut draw, content);
        let ws = crate::con::con_new(&mut t, &mut NullBackend, Some(content), None);
        t.con_mut(ws).r#type = ConType::CT_WORKSPACE;
        x_con_init(&mut t, &mut NullBackend, &mut x, &mut state, &mut draw, ws);
        t.focused = Some(ws);
        let frames: Vec<ConId> = vec![output, content, ws];
        (t, state, x, frames)
    }

    #[test]
    fn x_con_init_assigns_frame() {
        let (t, _state, _x, frames) = build_tree_with_frames();
        for con in frames {
            assert!(t.cons[con].frame.is_some(), "frame assigned");
        }
    }

    #[test]
    fn x_push_changes_restacks_on_order_change() {
        // A client is focused: x_push_changes only sets the X input focus for
        // a mapped con with a window (src/x.c:1405-1423), and only the first
        // focus change emits requests.
        let (mut t, mut state, mut x, a, ws, _window) = build_tree_with_client();
        let b = &mut NullBackend;
        let frames: Vec<ConId> = vec![
            t.cons[t.croot.unwrap()].nodes_head[1],
            t.cons[t.cons[t.croot.unwrap()].nodes_head[1]].nodes_head[0],
            ws,
        ];
        // First push: geometry configure + initial stack (below for first)
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, b, &mut x, &a, &mut state, &mut NullDraw, croot);
        let cfg_count_first = x.log.iter().filter(|l| l.starts_with("configure_window")).count();
        assert!(cfg_count_first >= 3, "each frame configured once, log={}", x.log.join(" | "));
        assert!(x.log.iter().any(|l| l.contains("STACK_MODE")));

        // Second push without changes: no additional configure requests.
        // (C: x_push_changes always ends with xcb_flush, so "flush" is the only
        // request the recording connection may see.)
        x.log.clear();
        x_push_changes(&mut t, b, &mut x, &a, &mut state, &mut NullDraw, croot);
        // EWMH bookkeeping (client list, active window, frame extents) is
        // pushed by the same call in C; only window geometry and stacking must
        // stay untouched here.
        let emitted: Vec<&String> = x
            .log
            .iter()
            .filter(|l| *l != "flush")
            .filter(|l| l.starts_with("configure_window"))
            .collect();
        assert!(
            emitted.is_empty(),
            "no-op push reconfigures nothing: {}",
            x.log.join(" | ")
        );

        // Change the rect of the ws frame -> exactly one configure request
        x.log.clear();
        t.con_mut(frames[2]).rect = Rect::new(0, 0, 1280, 760);
        x_push_changes(&mut t, b, &mut x, &a, &mut state, &mut NullDraw, croot);
        let cfg = x.log.iter().filter(|l| l.starts_with("configure_window")).count();
        assert_eq!(cfg, 1, "only the changed frame is reconfigured: {}", x.log.join(" | "));
    }

    /// A tree with one managed client window on the focused workspace, the
    /// state the focus/EWMH block of x_push_changes works on.
    fn build_tree_with_client() -> (Tree, XState, RecordingConn, Atoms, ConId, u32) {
        let (mut t, mut state, mut x, frames) = build_tree_with_frames();
        let a = Atoms::intern_all(&mut x);
        let ws = frames[2];
        let win = t.windows.insert(crate::data::Window {
            id: 0x7777,
            ..Default::default()
        });
        t.con_mut(ws).window = Some(win);
        t.con_mut(ws).mapped = true;
        t.focused = Some(ws);
        // The layouts tree_init/workspace_get would have assigned; dump_node
        // asserts on L_DEFAULT (C: src/dump.c asserts the same).
        t.con_mut(frames[0]).layout = Layout::L_OUTPUT;
        t.con_mut(frames[1]).layout = Layout::L_SPLITH;
        t.con_mut(ws).layout = Layout::L_SPLITH;
        // C: ewmh_setup_hints() created the support window at startup.
        crate::ewmh::ewmh_setup_hints(&mut x, &a);
        set_focused_id(XCB_WINDOW_NONE);
        set_last_focused(XCB_WINDOW_NONE);
        set_last_timestamp(42);
        crate::ipc::clear_pending_events();
        x.log.clear();
        (t, state, x, a, ws, 0x7777)
    }

    #[test]
    fn push_changes_sets_input_focus_and_ewmh_active_window() {
        // C: src/x.c:1428-1445 — xcb_set_input_focus(to_focus, last_timestamp)
        // then change_ewmh_focus() sets _NET_ACTIVE_WINDOW and
        // _NET_WM_STATE_FOCUSED.
        let (mut t, mut state, mut x, a, _ws, window) = build_tree_with_client();
        let ewmh = crate::ewmh::get_ewmh_window();
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);

        assert!(
            x.log.contains(&format!("set_input_focus({window:#x}, ts=42)")),
            "the client window gets the X focus: {}",
            x.log.join(" | ")
        );
        assert_eq!(focused_id(), window, "focused_id caches the focused window");
        assert_ne!(ewmh, 0, "the EWMH support window exists");

        let root = x.root;
        let active = x
            .written_properties
            .get(&(root, a.A__NET_ACTIVE_WINDOW))
            .expect("_NET_ACTIVE_WINDOW was written");
        assert_eq!(active, &window.to_le_bytes(), "_NET_ACTIVE_WINDOW = the client");
    }

    #[test]
    fn push_changes_unmaps_frames_of_unmapped_cons() {
        // C: src/x.c:1155-1182 + 1478 — a con that is not mapped anymore
        // (tree_render -> mark_unmapped) gets `unmap_now`, and
        // x_push_node_unmaps() unmaps its frame and withdraws the client.
        let (mut t, mut state, mut x, a, ws, window) = build_tree_with_client();
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);
        assert!(
            x.log.iter().any(|l| l == "map_window(0x7777)"),
            "the client is mapped: {}",
            x.log.join(" | ")
        );

        // C: mark_unmapped(con) before render_con for an invisible workspace.
        t.con_mut(ws).mapped = false;
        x.log.clear();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);

        assert!(
            x.log.iter().any(|l| l.starts_with("unmap_window(")),
            "the frame is unmapped: {}",
            x.log.join(" | ")
        );
        let wm_state = x
            .written_properties
            .get(&(window, a.A_WM_STATE))
            .expect("WM_STATE is set before the unmap");
        assert_eq!(wm_state[..4], 0u32.to_le_bytes(), "WM_STATE = WITHDRAWN");
        assert_eq!(t.cons[ws].ignore_unmap, 1, "ignore_unmap is increased");
    }

    #[test]
    fn push_changes_writes_empty_client_list_on_first_push() {
        // C: the first x_push_changes writes the hints even without a client
        // (every con_state is `initial`), so the root window carries an empty
        // _NET_CLIENT_LIST from startup on.
        let (mut t, mut state, mut x, frames) = build_tree_with_frames();
        let a = Atoms::intern_all(&mut x);
        t.con_mut(frames[2]).mapped = true;
        x.log.clear();
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);
        let root = x.root;
        assert_eq!(
            x.written_properties.get(&(root, a.A__NET_CLIENT_LIST)),
            Some(&Vec::new()),
            "an empty _NET_CLIENT_LIST is written"
        );
        assert_eq!(
            x.written_properties.get(&(root, a.A__NET_CLIENT_LIST_STACKING)),
            Some(&Vec::new())
        );
    }

    #[test]
    fn push_changes_writes_net_client_list() {
        // C: ewmh_update_client_list / _stacking over the managed windows.
        let (mut t, mut state, mut x, a, _ws, window) = build_tree_with_client();
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);

        let root = x.root;
        let list = x
            .written_properties
            .get(&(root, a.A__NET_CLIENT_LIST))
            .expect("_NET_CLIENT_LIST was written");
        assert_eq!(list, &window.to_le_bytes());
        let stacking = x
            .written_properties
            .get(&(root, a.A__NET_CLIENT_LIST_STACKING))
            .expect("_NET_CLIENT_LIST_STACKING was written");
        assert_eq!(stacking, &window.to_le_bytes());

        // Second push: the lists did not change, so nothing is rewritten.
        x.written_properties.remove(&(root, a.A__NET_CLIENT_LIST));
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);
        assert!(!x.written_properties.contains_key(&(root, a.A__NET_CLIENT_LIST)));
    }

    #[test]
    fn push_changes_sends_focus_ipc_event_once() {
        // C: src/x.c:1440 ipc_send_window_event("focus", focused) — only when
        // the focus actually moves (to_focus != last_focused).
        let (mut t, mut state, mut x, a, _ws, window) = build_tree_with_client();
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);

        let events = crate::ipc::take_pending_events();
        assert_eq!(events.len(), 1, "one focus event: {events:?}");
        assert_eq!(events[0].subscription, "window");
        assert_eq!(events[0].message_type, crate::ipc::I3_IPC_EVENT_WINDOW);
        assert!(
            events[0].payload.contains("\"change\":\"focus\""),
            "payload={}",
            events[0].payload
        );
        assert!(events[0].payload.contains(&format!("{}", window)));

        // The focus did not move again -> no second event.
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);
        assert!(crate::ipc::take_pending_events().is_empty());
    }

    #[test]
    fn push_changes_focuses_ewmh_window_without_a_client() {
        // C: src/x.c:1447-1457 — focused_id == XCB_NONE -> focus the EWMH
        // support window and clear _NET_ACTIVE_WINDOW.
        let (mut t, mut state, mut x, frames) = build_tree_with_frames();
        let a = Atoms::intern_all(&mut x);
        let ewmh = crate::ewmh::ewmh_setup_hints(&mut x, &a);
        set_focused_id(XCB_WINDOW_NONE);
        // C: a client was focused before (last_focused != XCB_NONE), so
        // change_ewmh_focus() really does clear _NET_ACTIVE_WINDOW.
        set_last_focused(0x7777);
        // C: the focused con is not mapped -> focus is invalidated.
        t.con_mut(frames[2]).mapped = false;
        x.log.clear();
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);

        assert!(
            x.log.contains(&format!("set_input_focus({ewmh:#x}, ts=0)")),
            "focus falls back to the EWMH window: {}",
            x.log.join(" | ")
        );
        assert_eq!(focused_id(), ewmh);
        let root = x.root;
        let active = x
            .written_properties
            .get(&(root, a.A__NET_ACTIVE_WINDOW))
            .expect("_NET_ACTIVE_WINDOW is cleared");
        assert_eq!(active, &0u32.to_le_bytes());
    }

    #[test]
    fn push_changes_sends_take_focus_for_globally_active_clients() {
        // C: src/x.c:1410-1422 — needs_take_focus && doesnt_accept_focus ->
        // WM_TAKE_FOCUS is sent instead of setting the input focus.
        let (mut t, mut state, mut x, a, ws, window) = build_tree_with_client();
        let win = t.cons[ws].window.unwrap();
        t.window_mut(win).needs_take_focus = true;
        t.window_mut(win).doesnt_accept_focus = true;
        x.log.clear();
        let croot = t.croot.unwrap();
        x_push_changes(&mut t, &mut NullBackend, &mut x, &a, &mut state, &mut NullDraw, croot);

        assert!(
            x.log.iter().any(|l| l.contains(&format!("send_client_message(win={window:#x}")) && l.contains("data32=[")),
            "WM_TAKE_FOCUS is sent: {}",
            x.log.join(" | ")
        );
        assert!(!x.log.iter().any(|l| l.starts_with("set_input_focus")), "no input focus for take-focus clients");
        assert_eq!(focused_id(), window);
    }

    #[test]
    fn stack_order_is_bottom_up() {
        let (t, _state, _x, frames) = build_tree_with_frames();
        let order = compute_stack_order(&t);
        // output frame, content frame, ws frame in tree order
        let expected: Vec<u32> = frames
            .iter()
            .filter_map(|c| t.cons[*c].frame.map(|f| f.id))
            .collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn x_window_kill_sends_delete() {
        let mut x = RecordingConn::new(0x50);
        let a = crate::xcb_util::Atoms::intern_all(&mut x);

        // C: a window which does not support WM_DELETE_WINDOW is killed the
        // hard way: KILL_WINDOW destroys the window, everything else kills the
        // X11 client which owns it.
        x.log.clear();
        x_window_kill(&mut x, &a, 0x42, KillWindow::DONT_KILL_WINDOW);
        assert_eq!(x.log.last().unwrap(), "kill_client(0x42)");
        x.log.clear();
        x_window_kill(&mut x, &a, 0x42, KillWindow::KILL_WINDOW);
        assert_eq!(x.log.last().unwrap(), "destroy_window(0x42)");

        // C: with WM_DELETE_WINDOW in WM_PROTOCOLS a WM_DELETE client message
        // is sent (data32[0] = A_WM_DELETE_WINDOW, data32[1] = CURRENT_TIME).
        x.props.insert(
            (0x43, a.A_WM_PROTOCOLS),
            crate::backend::PropertyReply {
                type_: 4, // XCB_ATOM_ATOM
                format: 32,
                value: a.A_WM_DELETE_WINDOW.to_le_bytes().to_vec(),
            },
        );
        x.log.clear();
        x_window_kill(&mut x, &a, 0x43, KillWindow::DONT_KILL_WINDOW);
        assert!(
            x.log.iter().any(|l| l.contains(&format!("data32=[{}, 0, 0, 0, 0]", a.A_WM_DELETE_WINDOW))),
            "log={}",
            x.log.join(" | ")
        );
        assert_eq!(x.log.last().unwrap(), "flush");
    }

    #[test]
    fn x_get_border_rectangles_counts() {
        let (mut t, _state, _x, frames) = build_tree_with_frames();
        let ws = frames[2];
        t.con_mut(ws).border_style = BorderStyle::BS_PIXEL;
        t.con_mut(ws).current_border_width = 2;
        let rects = x_get_border_rectangles(&t, ws);
        assert_eq!(rects.len(), 4, "pixel borders: top/right/bottom/left");
        t.con_mut(ws).border_style = BorderStyle::BS_NONE;
        assert!(x_get_border_rectangles(&t, ws).is_empty());
    }

    #[test]
    fn deco_params_cached_until_changed() {
        // C: x_draw_decoration skips the redraw when the cached params match
        let (mut t, _state, _x, frames) = build_tree_with_frames();
        let mut draw = NullDraw;
        let ws = frames[2];
        t.con_mut(ws).name = Some("ws".to_string());
        // C: containers whose height is 0 are skipped (src/x.c:478)
        t.con_mut(ws).rect = Rect::new(0, 0, 1280, 800);
        x_draw_decoration(&mut t, &mut draw, ws);
        let cached1 = t.cons[ws].deco_render_params.clone().unwrap();
        x_draw_decoration(&mut t, &mut draw, ws);
        let cached2 = t.cons[ws].deco_render_params.clone().unwrap();
        assert_eq!(cached1.border_style, cached2.border_style);
        assert_eq!(cached1.parent_layout, cached2.parent_layout);
    }

    #[test]
    fn real_connection_skips_without_display() {
        // X11 live test runs only with DISPLAY (Xvfb unavailable here).
        if std::env::var("DISPLAY").is_err() {
            eprintln!("skip: DISPLAY not set, live x_con_init test skipped");
            return;
        }
        assert!(crate::xcb_util::RustXConn::connect().is_some());
    }

    #[test]
    fn stack_mode_constants_match_xcb() {
        // C: XCB_STACK_MODE_ABOVE=0, BELOW=1
        assert_eq!(StackMode::ABOVE as u32, 0);
        assert_eq!(StackMode::BELOW as u32, 1);
    }

    #[test]
    fn x_reparent_child_moves_window() {
        let (mut t, mut state, mut x, frames) = build_tree_with_frames();
        // a client window in ws[2]
        let win = t.windows.insert(crate::data::Window { id: 0x99, ..Default::default() });
        t.window_index.insert(0x99, win);
        t.con_mut(frames[2]).window = Some(win);
        x_reparent_child(&mut t, &mut x, frames[1], frames[2]);
        let log = x.log.join(" | ");
        let frame1 = t.cons[frames[1]].frame.unwrap().id;
        assert!(log.contains(&format!("reparent_window(0x99 -> {frame1:#x} @ 0,0)")), "log={log}");
        assert!(log.contains("map_window(0x99)"));
        let _ = state;
    }
}
