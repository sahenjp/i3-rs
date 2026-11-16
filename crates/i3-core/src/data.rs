//! 1:1 port of i3's data structures.
//!
//! C: include/data.h
#![allow(non_camel_case_types)]

use std::collections::VecDeque;

use slotmap::new_key_type;

new_key_type! {
    /// Handle for a Con in `Tree::cons`. Corresponds to a `Con *` in C.
    pub struct ConId;
}

new_key_type! {
    /// Handle for a Window in `Tree::windows`. Corresponds to a `struct Window *` in C.
    pub struct WindowId;
}

/// C: include/data.h (xcb_window_t); XCB_WINDOW_NONE is 0.
pub const XCB_WINDOW_NONE: u32 = 0;
/// C: include/data.h (xcb_atom_t); XCB_ATOM_NONE is 0.
pub const XCB_ATOM_NONE: u32 = 0;

// The predefined atoms (C: xcb.h XCB_ATOM_*) the ICCCM code reads. Their
// numeric ids are fixed by the X11 protocol and are NOT the same as the
// interned _NET_*/i3 atoms.
/// C: XCB_ATOM_STRING
pub const XCB_ATOM_STRING: u32 = 31;
/// C: XCB_ATOM_WM_HINTS
pub const XCB_ATOM_WM_HINTS: u32 = 35;
/// C: XCB_ATOM_WM_CLIENT_MACHINE
pub const XCB_ATOM_WM_CLIENT_MACHINE: u32 = 36;
/// C: XCB_ATOM_WM_NAME
pub const XCB_ATOM_WM_NAME: u32 = 39;
/// C: XCB_ATOM_WM_NORMAL_HINTS
pub const XCB_ATOM_WM_NORMAL_HINTS: u32 = 40;
/// C: XCB_ATOM_WM_CLASS
pub const XCB_ATOM_WM_CLASS: u32 = 67;
/// C: XCB_ATOM_WM_TRANSIENT_FOR
pub const XCB_ATOM_WM_TRANSIENT_FOR: u32 = 68;

/******************************************************************************
 * Helper types (C: include/data.h:44ff)
 *****************************************************************************/

/// C: include/data.h:44 `typedef enum { D_LEFT, D_RIGHT, D_UP, D_DOWN } direction_t;`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    D_LEFT = 0,
    D_RIGHT = 1,
    D_UP = 2,
    D_DOWN = 3,
}
pub use Direction as direction_t;

/// C: include/data.h:46 `typedef enum { NO_ORIENTATION = 0, HORIZ, VERT } orientation_t;`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Orientation {
    NO_ORIENTATION = 0,
    HORIZ = 1,
    VERT = 2,
}
pub use Orientation as orientation_t;

/// C: include/data.h:48 `typedef enum { BEFORE, AFTER } position_t;`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Position {
    BEFORE = 0,
    AFTER = 1,
}
pub use Position as position_t;

/// C: include/data.h:50 `border_style_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum BorderStyle {
    #[default]
    BS_NONE = 0,
    BS_PIXEL = 1,
    BS_NORMAL = 2,
}
pub use BorderStyle as border_style_t;

/// C: include/floating.h:17 border_t
///
/// On which border was the dragging initiated? (a bitmask, 0 = none)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct border_t(pub u32);

impl border_t {
    pub const BORDER_LEFT: border_t = border_t(1 << 0);
    pub const BORDER_RIGHT: border_t = border_t(1 << 1);
    pub const BORDER_TOP: border_t = border_t(1 << 2);
    pub const BORDER_BOTTOM: border_t = border_t(1 << 3);
}

impl std::ops::BitOr for border_t {
    type Output = border_t;
    fn bitor(self, rhs: border_t) -> border_t {
        border_t(self.0 | rhs.0)
    }
}
impl std::ops::BitOrAssign for border_t {
    fn bitor_assign(&mut self, rhs: border_t) {
        self.0 |= rhs.0;
    }
}
impl std::ops::BitAnd for border_t {
    type Output = border_t;
    fn bitand(self, rhs: border_t) -> border_t {
        border_t(self.0 & rhs.0)
    }
}
impl border_t {
    /// C: corner & BORDER_LEFT — whether the flag is set.
    pub fn has(self, flag: border_t) -> bool {
        self.0 & flag.0 != 0
    }
}

/// C: include/tiling_drag.h:15 — tiling_drag_t.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum tiling_drag_t {
    TILING_DRAG_OFF = 0,
    #[default]
    TILING_DRAG_MODIFIER = 1,
    TILING_DRAG_TITLEBAR = 2,
    TILING_DRAG_MODIFIER_OR_TITLEBAR = 3,
}

/// C: include/data.h:58 kill_window_t
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KillWindow {
    DONT_KILL_WINDOW = 0,
    KILL_WINDOW = 1,
    KILL_CLIENT = 2,
}
pub use KillWindow as kill_window_t;

/// C: include/data.h:62 `adjacent_t` (bitmask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Adjacent(pub u32);
/// C: include/data.h:63
pub const ADJ_NONE: Adjacent = Adjacent(0);
/// C: include/data.h:64
pub const ADJ_LEFT_SCREEN_EDGE: Adjacent = Adjacent(1 << 0);
/// C: include/data.h:65
pub const ADJ_RIGHT_SCREEN_EDGE: Adjacent = Adjacent(1 << 1);
/// C: include/data.h:66
pub const ADJ_UPPER_SCREEN_EDGE: Adjacent = Adjacent(1 << 2);
/// C: include/data.h:67
pub const ADJ_LOWER_SCREEN_EDGE: Adjacent = Adjacent(1 << 4);
pub use Adjacent as adjacent_t;

impl std::ops::BitOr for Adjacent {
    type Output = Adjacent;
    fn bitor(self, rhs: Adjacent) -> Adjacent {
        Adjacent(self.0 | rhs.0)
    }
}
impl std::ops::BitOrAssign for Adjacent {
    fn bitor_assign(&mut self, rhs: Adjacent) {
        self.0 |= rhs.0;
    }
}
impl std::ops::BitAnd for Adjacent {
    type Output = Adjacent;
    fn bitand(self, rhs: Adjacent) -> Adjacent {
        Adjacent(self.0 & rhs.0)
    }
}

/// C: include/data.h:69 `smart_gaps_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SmartGaps {
    SMART_GAPS_OFF = 0,
    SMART_GAPS_ON = 1,
    SMART_GAPS_INVERSE_OUTER = 2,
}
pub use SmartGaps as smart_gaps_t;

/// C: include/data.h:73 `hide_edge_borders_mode_t` (bitmask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HideEdgeBordersMode(pub u32);
/// C: include/data.h:74
pub const HEBM_NONE: HideEdgeBordersMode = HideEdgeBordersMode(ADJ_NONE.0);
/// C: include/data.h:75
pub const HEBM_VERTICAL: HideEdgeBordersMode = HideEdgeBordersMode(ADJ_LEFT_SCREEN_EDGE.0 | ADJ_RIGHT_SCREEN_EDGE.0);
/// C: include/data.h:76
pub const HEBM_HORIZONTAL: HideEdgeBordersMode = HideEdgeBordersMode(ADJ_UPPER_SCREEN_EDGE.0 | ADJ_LOWER_SCREEN_EDGE.0);
/// C: include/data.h:77
pub const HEBM_BOTH: HideEdgeBordersMode = HideEdgeBordersMode(HEBM_VERTICAL.0 | HEBM_HORIZONTAL.0);
/// C: include/data.h:78
pub const HEBM_SMART: HideEdgeBordersMode = HideEdgeBordersMode(1 << 5);
/// C: include/data.h:79
pub const HEBM_SMART_NO_GAPS: HideEdgeBordersMode = HideEdgeBordersMode(1 << 6);
pub use HideEdgeBordersMode as hide_edge_borders_mode_t;

impl std::ops::BitAnd<HideEdgeBordersMode> for Adjacent {
    type Output = Adjacent;
    fn bitand(self, rhs: HideEdgeBordersMode) -> Adjacent {
        Adjacent(self.0 & rhs.0)
    }
}

/// C: include/data.h:81 `mark_mode_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkMode {
    MM_REPLACE = 0,
    MM_ADD = 1,
}
pub use MarkMode as mark_mode_t;

/// C: include/data.h:86 `layout_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Layout {
    #[default]
    L_DEFAULT = 0,
    L_STACKED = 1,
    L_TABBED = 2,
    L_DOCKAREA = 3,
    L_OUTPUT = 4,
    L_SPLITV = 5,
    L_SPLITH = 6,
}
pub use Layout as layout_t;

/// C: include/data.h:95 `input_type_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InputType {
    #[default]
    B_KEYBOARD = 0,
    B_MOUSE = 1,
}
pub use InputType as input_type_t;

/// C: include/data.h:102 `i3_xkb_group_mask_t` (bitmask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct I3XkbGroupMask(pub u32);
/// C: include/data.h:103
pub const I3_XKB_GROUP_MASK_ANY: I3XkbGroupMask = I3XkbGroupMask(0);
/// C: include/data.h:104
pub const I3_XKB_GROUP_MASK_1: I3XkbGroupMask = I3XkbGroupMask(1 << 0);
/// C: include/data.h:105
pub const I3_XKB_GROUP_MASK_2: I3XkbGroupMask = I3XkbGroupMask(1 << 1);
/// C: include/data.h:106
pub const I3_XKB_GROUP_MASK_3: I3XkbGroupMask = I3XkbGroupMask(1 << 2);
/// C: include/data.h:107
pub const I3_XKB_GROUP_MASK_4: I3XkbGroupMask = I3XkbGroupMask(1 << 3);
pub use I3XkbGroupMask as i3_xkb_group_mask_t;

impl std::ops::BitOr for I3XkbGroupMask {
    type Output = I3XkbGroupMask;
    fn bitor(self, rhs: I3XkbGroupMask) -> I3XkbGroupMask {
        I3XkbGroupMask(self.0 | rhs.0)
    }
}
impl std::ops::BitAnd for I3XkbGroupMask {
    type Output = I3XkbGroupMask;
    fn bitand(self, rhs: I3XkbGroupMask) -> I3XkbGroupMask {
        I3XkbGroupMask(self.0 & rhs.0)
    }
}

/// C: include/data.h:115 `typedef uint32_t i3_event_state_mask_t;`
pub type i3_event_state_mask_t = u32;

/// C: include/data.h:120 `warping_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Warping {
    POINTER_WARPING_OUTPUT = 0,
    POINTER_WARPING_NONE = 1,
}
pub use Warping as warping_t;

/// C: include/data.h:125 `struct gaps_t`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct gaps_t {
    pub inner: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

/// C: include/data.h:133 `gaps_mask_t` (bitmask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GapsMask(pub u32);
/// C: include/data.h:134
pub const GAPS_INNER: GapsMask = GapsMask(1 << 0);
/// C: include/data.h:135
pub const GAPS_TOP: GapsMask = GapsMask(1 << 1);
/// C: include/data.h:136
pub const GAPS_RIGHT: GapsMask = GapsMask(1 << 2);
/// C: include/data.h:137
pub const GAPS_BOTTOM: GapsMask = GapsMask(1 << 3);
/// C: include/data.h:138
pub const GAPS_LEFT: GapsMask = GapsMask(1 << 4);
/// C: include/data.h:139
pub const GAPS_VERTICAL: GapsMask = GapsMask(GAPS_TOP.0 | GAPS_BOTTOM.0);
/// C: include/data.h:140
pub const GAPS_HORIZONTAL: GapsMask = GapsMask(GAPS_RIGHT.0 | GAPS_LEFT.0);
/// C: include/data.h:141
pub const GAPS_OUTER: GapsMask = GapsMask(GAPS_VERTICAL.0 | GAPS_HORIZONTAL.0);
pub use GapsMask as gaps_mask_t;

impl std::ops::BitOr for GapsMask {
    type Output = GapsMask;
    fn bitor(self, rhs: GapsMask) -> GapsMask {
        GapsMask(self.0 | rhs.0)
    }
}
impl std::ops::BitAnd for GapsMask {
    type Output = GapsMask;
    fn bitand(self, rhs: GapsMask) -> GapsMask {
        GapsMask(self.0 & rhs.0)
    }
}

/// C: include/data.h:146 `focus_wrapping_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusWrapping {
    FOCUS_WRAPPING_OFF = 0,
    FOCUS_WRAPPING_ON = 1,
    FOCUS_WRAPPING_FORCE = 2,
    FOCUS_WRAPPING_WORKSPACE = 3,
}
pub use FocusWrapping as focus_wrapping_t;

/// C: include/data.h:166 `struct Rect`
///
/// x and y are `uint32_t` in C; all arithmetic in the port uses wrapping
/// semantics to reproduce C's unsigned overflow behaviour bug-for-bug.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Convenience constructor mirroring C's `(Rect){x, y, w, h}`.
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect { x, y, width, height }
    }
}

/// C: include/data.h:178 `struct reservedpx`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct reservedpx {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

/// C: include/data.h:186 `struct width_height`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct width_height {
    pub w: u32,
    pub h: u32,
}

/// Stub for `struct Colortriple` (X/cairo layer will provide the real type).
///
/// C: include/configuration.h (struct Colortriple)
#[derive(Debug, Clone, Copy, Default)]
pub struct ColortripleStub;

/// Stub for `color_t`.
///
/// C: include/libi3.h (typedef struct color_t)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct color_t_stub {
    pub red: u16,
    pub green: u16,
    pub blue: u16,
    pub alpha: u16,
}

/// Stub for an X/cairo-backed icon (`cairo_surface_t *`).
///
/// C: include/data.h (Window.icon)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpaqueIcon {
    pub handle: u64,
}

/// Stub for an opaque pointer (e.g. `SnLauncherContext *`).
///
/// C: include/data.h:287 (Startup_Sequence.context)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpaqueHandle {
    pub handle: u64,
}

/// Stub for the X11 frame surface (`surface_t`).
///
/// C: include/xcb.h (struct surface) — only the `id` member (frame window)
/// is modelled here so that con_by_frame_id() works.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameSurface {
    pub id: u32,
}

/// Stub for `struct ev_timer *` (urgency timer).
///
/// C: src/workspace.c (workspace_defer_update_urgent_hint_cb wiring).
/// `data` is the Con the timer belongs to; `after`/`repeat` mirror the
/// ev_timer init values. The real event loop (calloop) will replace this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimerHandle {
    pub data: ConId,
    pub after: f64,
    pub repeat: f64,
}

/// C: sys/time.h `struct timeval` (only used for the urgency timestamp).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// C: include/data.h:191 `struct deco_render_params`
#[derive(Debug, Clone, Default)]
pub struct deco_render_params {
    pub color: Option<ColortripleStub>,
    pub border_style: i32,
    pub con_rect: width_height,
    pub con_window_rect: width_height,
    pub con_deco_rect: Rect,
    pub background: color_t_stub,
    pub parent_layout: Layout,
    pub con_is_leaf: bool,
}

/// C: include/data.h:251 `struct Workspace_Assignment`
#[derive(Debug, Clone, Default)]
pub struct Workspace_Assignment {
    pub name: String,
    pub output: Option<String>,
    pub gaps: gaps_t,
    pub gaps_mask: GapsMask,
}

/// C: include/data.h:271 `ignore_event_sequence_t`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ignore_event_sequence_t {
    pub value: u32,
}

/// C: include/data.h:285 `struct Ignore_Event`
#[derive(Debug, Clone, Default)]
pub struct Ignore_Event {
    pub sequence: ignore_event_sequence_t,
    pub response_type: i32,
    pub added: i64,
}

/// C: include/data.h:297 `struct Startup_Sequence`
#[derive(Debug, Clone, Default)]
pub struct Startup_Sequence {
    /// startup ID for this sequence, generated by libstartup-notification
    pub id: String,
    /// workspace on which this startup was initiated
    pub workspace: Option<String>,
    /// libstartup-notification context for this launch (stub)
    pub context: OpaqueHandle,
    /// time at which this sequence should be deleted (after it was marked as completed)
    pub delete_at: i64,
}

/// C: src/regex.c:26 `struct regex` — pattern plus compiled PCRE2 regex.
#[derive(Debug, Clone)]
pub struct regex {
    pub pattern: String,
    pub regex: Option<pcre2::bytes::Regex>,
}

impl regex {
    /// C: src/regex.c:26 `regex_new` (returns a pattern-only struct when the
    /// pattern cannot be compiled; C would return NULL — callers treat a
    /// compiled regex as required, so we keep the pattern for diagnostics).
    pub fn new(pattern: &str) -> regex {
        // C: src/regex.c:28-31 — "options = PCRE2_UTF; options |= PCRE2_UCP",
        // so \w / \d / \b are Unicode-aware (t/119-match matches a UTF-8
        // title like "ä 3" with ^\w [3]$).
        let compiled = pcre2::bytes::RegexBuilder::new()
            .utf(true)
            .ucp(true)
            .build(pattern)
            .ok();
        regex {
            pattern: pattern.to_string(),
            regex: compiled,
        }
    }

    /// C: src/regex.c:62 `regex_matches`
    pub fn matches(&self, input: &str) -> bool {
        let re = match &self.regex {
            Some(re) => re,
            None => return false,
        };
        re.is_match(input.as_bytes()).unwrap_or(false)
    }
}

/// C: include/data.h:326 `struct Binding_Keycode`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Binding_Keycode {
    pub keycode: u8,
    pub modifiers: i3_event_state_mask_t,
}

/******************************************************************************
 * Major types (C: include/data.h:345ff)
 *****************************************************************************/

/// C: include/data.h:316 (release enum inside `struct Binding`)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BindingRelease {
    /// This binding will only be executed upon KeyPress events
    #[default]
    B_UPON_KEYPRESS = 0,
    /// This binding will be executed either upon a KeyRelease event, or…
    B_UPON_KEYRELEASE = 1,
    /// …upon a KeyRelease event, even if the modifiers don't match.
    B_UPON_KEYRELEASE_IGNORE_MODS = 2,
}

/// C: include/data.h:349 `struct Binding`
#[derive(Debug, Clone, Default)]
pub struct Binding {
    pub input_type: InputType,
    pub release: BindingRelease,
    pub border: bool,
    pub whole_window: bool,
    pub exclude_titlebar: bool,
    pub keycode: u32,
    pub event_state_mask: i3_event_state_mask_t,
    pub symbol: Option<String>,
    pub keycodes_head: VecDeque<Binding_Keycode>,
    pub command: String,
}

/// C: include/data.h:396 `struct Autostart`
#[derive(Debug, Clone, Default)]
pub struct Autostart {
    pub command: String,
    pub no_startup_id: bool,
}

/// C: include/data.h:406 `struct output_name`
#[derive(Debug, Clone, Default)]
pub struct output_name {
    pub name: String,
}

/// C: include/data.h:415 `struct xoutput` (typedef Output)
#[derive(Debug, Clone, Default)]
pub struct Output {
    /// Output id, so that we can requery the output directly later
    pub id: u32,
    /// Whether the output is currently active (has a CRTC attached with a valid mode)
    pub active: bool,
    /// Internal flags, necessary for querying RandR screens (happens in two stages)
    pub changed: bool,
    pub to_be_disabled: bool,
    pub primary: bool,
    /// List of names for the output. An output always has at least one name;
    /// the first name is considered the primary one.
    pub names_head: VecDeque<output_name>,
    /// Pointer to the Con which represents this output
    pub con: Option<ConId>,
    /// x, y, width, height
    pub rect: Rect,
}

/// C: src/randr.h `output_close_far_t`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCloseFar {
    CLOSEST_OUTPUT = 0,
    FARTHEST_OUTPUT = 1,
}
pub use OutputCloseFar as output_close_far_t;

/// C: include/data.h:451 (dock enum inside `struct Window`)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WindowDock {
    #[default]
    W_NODOCK = 0,
    W_DOCK_TOP = 1,
    W_DOCK_BOTTOM = 2,
}

/// C: include/data.h:439 `struct Window` (typedef i3Window)
#[derive(Debug, Clone, Default)]
pub struct Window {
    pub id: u32,
    /// Holds the xcb_window_t (just an ID) for the leader window (logical parent for toolwindows and similar floating windows)
    pub leader: u32,
    pub transient_for: u32,
    /// Pointers to the Assignments which were already ran for this Window (assignments run only once).
    /// Indices into `Tree::assignments`.
    pub nr_assignments: u32,
    pub ran_assignments: VecDeque<usize>,
    pub class_class: Option<String>,
    pub class_instance: Option<String>,
    /// The name of the window. (i3String* in C; TODO: real i3String with markup flag)
    pub name: Option<String>,
    /// The WM_WINDOW_ROLE of this window (for example, the pidgin buddy window sets "buddy list").
    pub role: Option<String>,
    /// WM_CLIENT_MACHINE of the window
    pub machine: Option<String>,
    /// Flag to force re-rendering the decoration upon changes
    pub name_x_changed: bool,
    /// Whether the application used _NET_WM_NAME
    pub uses_net_wm_name: bool,
    /// Whether the application needs to receive WM_TAKE_FOCUS
    pub needs_take_focus: bool,
    /// Whether this window accepts focus. We store this inverted so that the default will be 'accepts focus'.
    pub doesnt_accept_focus: bool,
    /// The _NET_WM_WINDOW_TYPE for this window.
    pub window_type: u32,
    /// The _NET_WM_DESKTOP for this window.
    pub wm_desktop: u32,
    /// Whether the window says it is a dock window
    pub dock: WindowDock,
    /// When this window was marked urgent. 0 means not urgent
    pub urgent: timeval,
    /// Pixels the window reserves. left/right/top/bottom
    pub reserved: reservedpx,
    /// Depth of the window
    pub depth: u16,
    /// the wanted size of the window, used in combination with size increments (see below).
    pub base_width: i32,
    pub base_height: i32,
    /// minimum increment size specified for the window (in pixels)
    pub width_increment: i32,
    pub height_increment: i32,
    /// Minimum size specified for the window.
    pub min_width: i32,
    pub min_height: i32,
    /// Maximum size specified for the window.
    pub max_width: i32,
    pub max_height: i32,
    /// aspect ratio from WM_NORMAL_HINTS (MPlayer uses this for example)
    pub min_aspect_ratio: f64,
    pub max_aspect_ratio: f64,
    /// Window icon, as Cairo surface (stub)
    pub icon: Option<OpaqueIcon>,
    /// The window has a nonrectangular shape.
    pub shaped: bool,
    /// The window has a nonrectangular input shape.
    pub input_shaped: bool,
    /// Time when the window became managed. Used to determine whether a window should be swallowed after initial management.
    pub managed_since: i64,
    /// The window has been swallowed.
    pub swallowed: bool,
}

/// C: include/data.h:527 (urgent enum inside `struct Match`)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MatchUrgency {
    U_DONTCHECK = -1,
    #[default]
    U_LATEST = 0,
    U_OLDEST = 1,
}

/// C: include/data.h:532 (dock enum inside `struct Match`)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MatchDock {
    M_DONTCHECK = -1,
    #[default]
    M_NODOCK = 0,
    M_DOCK_ANY = 1,
    M_DOCK_TOP = 2,
    M_DOCK_BOTTOM = 3,
}

/// C: include/data.h:540 (window_mode enum inside `struct Match`)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MatchWindowMode {
    #[default]
    WM_ANY = 0,
    WM_TILING_AUTO = 1,
    WM_TILING_USER = 2,
    WM_TILING = 3,
    WM_FLOATING_AUTO = 4,
    WM_FLOATING_USER = 5,
    WM_FLOATING = 6,
}

/// C: include/data.h:559 (insert_where enum inside `struct Match`)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MatchInsertWhere {
    /// the matched container will be replaced by the window (layout saving)
    #[default]
    M_HERE = 0,
    /// the matched container will be inserted in the target_ws.
    M_ASSIGN_WS = 1,
    /// the window will be inserted as a child of the matched container (dockareas)
    M_BELOW = 2,
}

/// C: include/data.h:503 `struct Match`
#[derive(Debug, Clone, Default)]
pub struct Match {
    /// Set if a criterion was specified incorrectly.
    pub error: Option<String>,
    pub title: Option<Box<regex>>,
    pub application: Option<Box<regex>>,
    pub class: Option<Box<regex>>,
    pub instance: Option<Box<regex>>,
    pub mark: Option<Box<regex>>,
    pub window_role: Option<Box<regex>>,
    pub workspace: Option<Box<regex>>,
    pub machine: Option<Box<regex>>,
    pub window_type: u32,
    pub urgent: MatchUrgency,
    pub dock: MatchDock,
    pub id: u32,
    pub window_mode: MatchWindowMode,
    pub con_id: Option<ConId>,
    pub match_all_windows: bool,
    pub insert_where: MatchInsertWhere,
    /// Whether this match was generated when restarting i3 inplace.
    pub restart_mode: bool,
}

/// C: include/data.h:583 (type bitmask inside `struct Assignment`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AssignmentType(pub u32);
/// C: include/data.h:585
pub const A_ANY: AssignmentType = AssignmentType(0);
/// C: include/data.h:586
pub const A_COMMAND: AssignmentType = AssignmentType(1 << 0);
/// C: include/data.h:587
pub const A_TO_WORKSPACE: AssignmentType = AssignmentType(1 << 1);
/// C: include/data.h:588
pub const A_NO_FOCUS: AssignmentType = AssignmentType(1 << 2);
/// C: include/data.h:589
pub const A_TO_WORKSPACE_NUMBER: AssignmentType = AssignmentType(1 << 3);
/// C: include/data.h:590
pub const A_TO_OUTPUT: AssignmentType = AssignmentType(1 << 4);
pub use AssignmentType as assignment_type_t;

impl std::ops::BitAnd for AssignmentType {
    type Output = AssignmentType;
    fn bitand(self, rhs: AssignmentType) -> AssignmentType {
        AssignmentType(self.0 & rhs.0)
    }
}
impl std::ops::BitOr for AssignmentType {
    type Output = AssignmentType;
    fn bitor(self, rhs: AssignmentType) -> AssignmentType {
        AssignmentType(self.0 | rhs.0)
    }
}

/// C: include/data.h:593 `struct Assignment`
///
/// The C `dest` member is a union of `command` / `workspace` / `output`
/// (all `char *`); the active member is selected by `type`, so a single
/// string is stored here.
#[derive(Debug, Clone, Default)]
pub struct Assignment {
    pub type_: AssignmentType,
    /// the criteria to check if a window matches
    pub match_: Match,
    /// destination workspace/command/output, depending on the type
    pub dest: Option<String>,
}

/// C: include/data.h:601 `fullscreen_mode_t`
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FullscreenMode {
    #[default]
    CF_NONE = 0,
    CF_OUTPUT = 1,
    CF_GLOBAL = 2,
}
pub use FullscreenMode as fullscreen_mode_t;

/// C: include/data.h:605 `struct mark_t`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct mark_t {
    pub name: String,
}

/// The `type` enum of `struct Con` (anonymous in C).
///
/// C: include/data.h:615 (enum inside struct Con)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ConType {
    #[default]
    CT_ROOT = 0,
    CT_OUTPUT = 1,
    CT_CON = 2,
    CT_FLOATING_CON = 3,
    CT_WORKSPACE = 4,
    CT_DOCKAREA = 5,
}

/// The `floating` enum of `struct Con` (anonymous in C).
///
/// C: include/data.h:795 (enum inside struct Con). The order of the values is
/// important because the C code checks `floating >= FLOATING_AUTO_ON`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum FloatingState {
    #[default]
    FLOATING_AUTO_OFF = 0,
    FLOATING_USER_OFF = 1,
    FLOATING_AUTO_ON = 2,
    FLOATING_USER_ON = 3,
}
pub use FloatingState as floating_state_t;

/// The `scratchpad_state` enum of `struct Con` (anonymous in C).
///
/// C: include/data.h:826 (enum inside struct Con)
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScratchpadState {
    /// Not a scratchpad window.
    #[default]
    SCRATCHPAD_NONE = 0,
    /// Just moved to scratchpad, not resized by the user yet.
    /// Window will be auto-centered and sized appropriately.
    SCRATCHPAD_FRESH = 1,
    /// The user changed position/size of the scratchpad window.
    SCRATCHPAD_CHANGED = 2,
}

/// C: include/data.h:611 `struct Con`
///
/// The `nodes` / `focused` / `all_cons` / `floating_windows` TAILQ_ENTRY
/// members of the C struct are modelled by the parent's VecDeques
/// (`nodes_head`, `focus_head`, `floating_head`) and `Tree::all_cons`.
/// The `on_remove_child` function pointer is replaced by a direct call to
/// `con::con_on_remove_child` (the only implementation in C).
#[derive(Debug, Clone, Default)]
pub struct Con {
    pub mapped: bool,
    /// Should this container be marked urgent? This gets set when the window inside this container (if any) sets the urgency hint, for example.
    pub urgent: bool,
    /// This counter contains the number of UnmapNotify events for this container (or, more precisely, for its ->frame) which should be ignored.
    pub ignore_unmap: u8,
    /// The surface used for the frame window. (stub; X layer fills it)
    pub frame: Option<FrameSurface>,
    pub frame_buffer: Option<FrameSurface>,
    pub pixmap_recreated: bool,
    pub r#type: ConType,
    /// the workspace number, if this Con is of type CT_WORKSPACE and the workspace is not a named workspace (for named workspaces, num == -1)
    pub num: i32,
    /// Only applicable for containers of type CT_WORKSPACE.
    pub gaps: gaps_t,
    pub parent: Option<ConId>,
    /// The position and size for this con. These coordinates are absolute. Note that the rect of a container does not include the decoration.
    pub rect: Rect,
    /// The position and size of the actual client window. These coordinates are relative to the container's rect.
    pub window_rect: Rect,
    /// The position and size of the container's decoration. These coordinates are relative to the container's parent's rect.
    pub deco_rect: Rect,
    /// the geometry this window requested when getting mapped
    pub geometry: Rect,
    pub name: Option<String>,
    /// The format with which the window's name should be displayed.
    pub title_format: Option<String>,
    /// Whether the window icon should be displayed, and with what padding. -1 means display no window icon (default behavior), 0 means display without any padding, 1 means display with 1 pixel of padding and so on.
    pub window_icon_padding: i32,
    /// a sticky-group is an identifier which bundles several containers to a group. The contents are shared between all of them, that is they are displayed on whichever of the containers is currently visible
    pub sticky_group: Option<String>,
    /// user-definable marks to jump to this container later
    pub marks_head: VecDeque<mark_t>,
    /// cached to decide whether a redraw is needed
    pub mark_changed: bool,
    pub percent: f64,
    /// the x11 border pixel attribute
    pub border_width: i32,
    pub current_border_width: i32,
    pub window: Option<WindowId>,
    /// timer used for disabling urgency (stub)
    pub urgency_timer: Option<TimerHandle>,
    /// Cache for the decoration rendering
    pub deco_render_params: Option<Box<deco_render_params>>,
    /// Only workspace-containers can have floating clients
    pub floating_head: VecDeque<ConId>,
    pub nodes_head: VecDeque<ConId>,
    pub focus_head: VecDeque<ConId>,
    pub swallow_head: VecDeque<Match>,
    pub fullscreen_mode: FullscreenMode,
    /// Whether this window should stick to the glass. This corresponds to the _NET_WM_STATE_STICKY atom and will only be respected if the window is floating.
    pub sticky: bool,
    /// layout is the layout of this container: one of split[v|h], stacked or tabbed.
    pub layout: Layout,
    /// last_split_layout is one of splitv or splith to support the old "layout default" command.
    pub last_split_layout: Layout,
    /// workspace_layout is only for type == CT_WORKSPACE cons.
    pub workspace_layout: Layout,
    pub border_style: BorderStyle,
    /// When the border style of a con changes because of motif hints, we don't want to set more decoration that the user wants.
    pub max_user_border_style: BorderStyle,
    /// floating? (= not in tiling layout) This cannot be simply a bool because we want to keep track of whether the status was set by the application (by setting _NET_WM_WINDOW_TYPE appropriately) or by the user.
    pub floating: FloatingState,
    pub scratchpad_state: ScratchpadState,
    /// The ID of this container before restarting. Necessary to correctly interpret back-references in the JSON (such as the focus stack).
    pub old_id: i32,
    /// Depth of the container window
    pub depth: u16,
    /// The colormap for this con if a custom one is used.
    pub colormap: u32,
}

/// C: include/configuration.h:271 `popup_during_fullscreen` enum
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PopupDuringFullscreen {
    #[default]
    PDF_LEAVE_FULLSCREEN = 0,
    PDF_IGNORE = 1,
    PDF_SMART = 2,
    PDF_ALL = 3,
}

/// C: include/configuration.h:199-208 `focus_on_window_activation` enum
///
/// The port keeps one enum per layer (like PopupDuringFullscreen):
/// config.rs has the `_t` variant that the config parser fills,
/// config::apply_to_tree maps it into this tree-layer copy.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FocusOnWindowActivation {
    /// Focus if the target workspace is visible, set urgency hint otherwise.
    #[default]
    FOWA_SMART = 0,
    /// Always set the urgency hint.
    FOWA_URGENT = 1,
    /// Always focus the window.
    FOWA_FOCUS = 2,
    /// Ignore the request (no focus, no urgency hint).
    FOWA_NONE = 3,
}

/// Subset of the C `config` global (src/config.c) that the tree layer reads.
///
/// C: include/configuration.h (struct Config, fields kept 1:1)
#[derive(Debug, Clone)]
pub struct Config {
    pub default_border: BorderStyle,
    pub default_floating_border: BorderStyle,
    pub default_border_width: i32,
    pub default_floating_border_width: i32,
    pub default_orientation: Orientation,
    pub default_layout: Layout,
    pub focus_wrapping: FocusWrapping,
    /// C: config.disable_focus_follows_mouse (src/handlers.c:95/175/219 —
    /// focus_follows_mouse no must not focus on EnterNotify/MotionNotify or
    /// when the pointer crosses output boundaries).
    pub disable_focus_follows_mouse: bool,
    /// C: config.mouse_warping (include/configuration.h:136) — with
    /// POINTER_WARPING_NONE the pointer is never warped (src/x.c:1553).
    pub mouse_warping: Warping,
    pub hide_edge_borders: HideEdgeBordersMode,
    pub smart_gaps: SmartGaps,
    pub gaps: gaps_t,
    pub popup_during_fullscreen: PopupDuringFullscreen,
    /// C: config.focus_on_window_activation (src/handlers.c:1289 reads it for
    /// _NET_ACTIVE_WINDOW style stacking requests)
    pub focus_on_window_activation: FocusOnWindowActivation,
    /// C: config.workspace_auto_back_and_forth (src/commands.c:104 and
    /// :406 — "workspace" / "focus workspace" switch back when the target is
    /// already focused)
    pub workspace_auto_back_and_forth: bool,
    pub workspace_urgency_timer: f64,
    /// C: config.floating_minimum_width/height, floating_maximum_width/height
    /// (include/configuration.h). 0 means "use the sane defaults / the combined
    /// output size", -1 disables the respective fixup
    /// (src/floating.c:185-229).
    pub floating_minimum_width: i32,
    pub floating_minimum_height: i32,
    pub floating_maximum_width: i32,
    pub floating_maximum_height: i32,
    /// C: config.font.height (subset of the font config)
    pub font_height: u32,
    /// C: config.floating_modifier (include/configuration.h) — the modifier
    /// which needs to be pressed in combination with the mouse buttons to
    /// move/resize floating windows (src/click.c route_click).
    pub floating_modifier: u32,
    /// C: config.swap_modifier — pressed together with floating_modifier to
    /// swap containers during a tiling drag (src/tiling_drag.c).
    pub swap_modifier: u32,
    /// C: config.tiling_drag (src/click.c).
    pub tiling_drag: tiling_drag_t,
    /// C: config.client.focused.indicator.colorpixel — the color of the
    /// tiling-drag drop indicator (src/tiling_drag.c:276).
    pub client_focused_indicator: u32,
    /// C: config.client.focused.border.colorpixel — the color of the resize
    /// bar (src/resize.c:232).
    pub client_focused_border: u32,
}

impl Default for Config {
    /// Defaults mirror C's memset(0) + the assignments in
    /// C: src/config.c:211-229 (focus_wrapping = FOCUS_WRAPPING_ON,
    /// default_border = BS_NORMAL, …).
    fn default() -> Config {
        Config {
            default_border: BorderStyle::BS_NORMAL,
            default_floating_border: BorderStyle::BS_NORMAL,
            default_border_width: 2,
            default_floating_border_width: 2,
            default_orientation: Orientation::NO_ORIENTATION,
            default_layout: Layout::L_DEFAULT,
            focus_wrapping: FocusWrapping::FOCUS_WRAPPING_ON,
            disable_focus_follows_mouse: false,
            // C: memset(0) — warping_t 0 is POINTER_WARPING_OUTPUT.
            mouse_warping: Warping::POINTER_WARPING_OUTPUT,
            hide_edge_borders: HideEdgeBordersMode(0),
            smart_gaps: SmartGaps::SMART_GAPS_OFF,
            gaps: gaps_t::default(),
            popup_during_fullscreen: PopupDuringFullscreen::PDF_LEAVE_FULLSCREEN,
            // C: src/config.c:213 — focus_on_window_activation = FOWA_SMART
            focus_on_window_activation: FocusOnWindowActivation::FOWA_SMART,
            // C: memset(0) — workspace_auto_back_and_forth defaults to false
            workspace_auto_back_and_forth: false,
            workspace_urgency_timer: 0.5,
            // C: memset(0) — 0 keeps the sane fixups of src/floating.c active.
            floating_minimum_width: 0,
            floating_minimum_height: 0,
            floating_maximum_width: 0,
            floating_maximum_height: 0,
            font_height: 15,
            // C: memset(0) — no floating modifier bound until the config sets
            // one (src/config.c:211-229 does not assign it).
            floating_modifier: 0,
            swap_modifier: 0,
            // C: config.tiling_drag defaults to TILING_DRAG_MODIFIER
            // (src/config.c:224).
            tiling_drag: tiling_drag_t::TILING_DRAG_MODIFIER,
            // C: black until the config is loaded (colorpixel 0 = black).
            client_focused_indicator: 0,
            client_focused_border: 0,
        }
    }
}
