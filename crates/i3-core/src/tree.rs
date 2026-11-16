//! 1:1 port of src/tree.c — everything that primarily modifies the layout
//! tree data structure.
//!
//! C: src/tree.c
//!
//! The C globals `croot`, `focused`, `all_cons` (src/tree.c:16-18) are
//! fields of `Tree`. The C globals from other files that the tree layer
//! touches (outputs, assignments, ws_assignments, previous_workspace_name,
//! binding_workspace_names, config) are Tree fields as well so that the
//! layer stays X-free and unit-testable.

use std::collections::{HashMap, VecDeque};

use slotmap::SlotMap;

use crate::backend::ConBackend;
use crate::con::{
    con_activate, con_detach, con_fix_percent, con_fullscreen_permits_focusing,
    con_get_fullscreen_con, con_get_workspace, con_is_floating, con_is_split, con_new,
    con_next_focused, con_num_children, con_orientation, con_descend_focused,
    con_descend_tiling_focused, con_on_remove_child, con_set_urgency, con_update_parents_urgency,
    ConId,
};
use crate::data::{
    Assignment, Binding, Con, ConType, Config, Direction, FullscreenMode, Layout, Output, Rect,
    Workspace_Assignment, Window, WindowId,
};
use crate::output::{get_output_containing, get_output_next, output_get_content};
use crate::data::OutputCloseFar;
use crate::render::render_con;
use crate::util::{orientation_from_direction, position_from_direction};
use crate::workspace::{workspace_is_visible, workspace_show, workspace_update_urgent_flag};

/// The layout tree plus every C global the tree layer uses.
///
/// C: src/tree.c:16 `Con *croot`, `Con *focused`,
///    src/tree.c:18 `struct all_cons_head all_cons`
#[derive(Debug, Default)]
pub struct Tree {
    /// All containers (the backing store behind every `Con *`).
    pub cons: SlotMap<ConId, Con>,
    /// All managed windows (the backing store behind every `i3Window *`).
    pub windows: SlotMap<WindowId, Window>,
    /// Reverse-lookup index xcb_window_t -> WindowId.
    pub window_index: HashMap<u32, WindowId>,
    /// C: src/tree.c:16 `croot`
    pub croot: Option<ConId>,
    /// C: src/tree.c:17 `focused`
    pub focused: Option<ConId>,
    /// C: src/tree.c:18 `all_cons` (insertion-ordered like the TAILQ)
    pub all_cons: VecDeque<ConId>,
    /// C: src/assignments.c `assignments`
    pub assignments: VecDeque<Assignment>,
    /// C: src/config.c `ws_assignments`
    pub ws_assignments: VecDeque<Workspace_Assignment>,
    /// C: src/randr.c `outputs`
    pub outputs: VecDeque<Output>,
    /// C: src/workspace.c:20 `previous_workspace_name`
    pub previous_workspace_name: Option<String>,
    /// C: src/main.c:86 `bindings` (the currently active set of key bindings)
    pub bindings: VecDeque<Binding>,
    /// C: src/workspace.c:23 `binding_workspace_names`
    pub binding_workspace_names: Vec<String>,
    /// C: src/config.c `config`
    pub config: Config,
    /// C: src/config_directives.c `struct barconfigs_head barconfigs` — the
    /// parsed `bar { … }` blocks. In C the list is a global that
    /// config_directives.c fills during parsing; the commands (commands.c:2207
    /// cmd_bar_mode) and the IPC layer (ipc.c:1109 GET_BAR_CONFIG) read and
    /// write the very same list, so the port keeps it next to the other C
    /// globals in the tree.
    pub barconfigs: VecDeque<crate::config::Barconfig>,
    /// C: src/x.c `root_depth`
    pub root_depth: u16,
}

impl Tree {
    /// A fresh, empty tree (C has no equivalent: the globals start zeroed).
    pub fn new() -> Tree {
        Tree {
            config: Config::default(),
            root_depth: 24,
            ..Default::default()
        }
    }

    /// Borrow a Con (the `Con *` analog).
    pub fn con(&self, id: ConId) -> &Con {
        &self.cons[id]
    }

    /// Mutably borrow a Con.
    pub fn con_mut(&mut self, id: ConId) -> &mut Con {
        &mut self.cons[id]
    }

    /// Borrow a Window (the `i3Window *` analog).
    pub fn window(&self, id: WindowId) -> &Window {
        &self.windows[id]
    }

    /// Mutably borrow a Window.
    pub fn window_mut(&mut self, id: WindowId) -> &mut Window {
        &mut self.windows[id]
    }

    // ------------------------------------------------------------------
    // TAILQ emulation helpers. These stand in for the TAILQ_* macros that
    // operate on Con->nodes_head / focus_head / floating_head.
    // ------------------------------------------------------------------

    /// TAILQ_NEXT(con, nodes)
    pub fn nodes_next(&self, con: ConId) -> Option<ConId> {
        let parent = self.cons[con].parent?;
        let nodes = &self.cons[parent].nodes_head;
        let idx = nodes.iter().position(|&c| c == con)?;
        nodes.get(idx + 1).copied()
    }

    /// TAILQ_PREV(con, nodes_head, nodes)
    pub fn nodes_prev(&self, con: ConId) -> Option<ConId> {
        let parent = self.cons[con].parent?;
        let nodes = &self.cons[parent].nodes_head;
        let idx = nodes.iter().position(|&c| c == con)?;
        if idx == 0 { None } else { nodes.get(idx - 1).copied() }
    }

    /// TAILQ_NEXT(con, focused)
    pub fn focus_next(&self, con: ConId) -> Option<ConId> {
        let parent = self.cons[con].parent?;
        let focus = &self.cons[parent].focus_head;
        let idx = focus.iter().position(|&c| c == con)?;
        focus.get(idx + 1).copied()
    }

    /// TAILQ_PREV(con, focus_head, focused)
    pub fn focus_prev(&self, con: ConId) -> Option<ConId> {
        let parent = self.cons[con].parent?;
        let focus = &self.cons[parent].focus_head;
        let idx = focus.iter().position(|&c| c == con)?;
        if idx == 0 { None } else { focus.get(idx - 1).copied() }
    }

    /// TAILQ_NEXT(con, floating_windows)
    pub fn floating_next(&self, con: ConId) -> Option<ConId> {
        let parent = self.cons[con].parent?;
        let fl = &self.cons[parent].floating_head;
        let idx = fl.iter().position(|&c| c == con)?;
        fl.get(idx + 1).copied()
    }

    /// TAILQ_PREV(con, floating_head, floating_windows)
    pub fn floating_prev(&self, con: ConId) -> Option<ConId> {
        let parent = self.cons[con].parent?;
        let fl = &self.cons[parent].floating_head;
        let idx = fl.iter().position(|&c| c == con)?;
        if idx == 0 { None } else { fl.get(idx - 1).copied() }
    }

    fn dq_remove(dq: &mut VecDeque<ConId>, con: ConId) {
        if let Some(idx) = dq.iter().position(|&c| c == con) {
            dq.remove(idx);
        }
    }

    /// TAILQ_REMOVE(&(parent->nodes_head), con, nodes)
    pub fn nodes_remove(&mut self, parent: ConId, con: ConId) {
        Self::dq_remove(&mut self.cons[parent].nodes_head, con);
    }

    /// TAILQ_REMOVE(&(parent->focus_head), con, focused)
    pub fn focus_remove(&mut self, parent: ConId, con: ConId) {
        Self::dq_remove(&mut self.cons[parent].focus_head, con);
    }

    /// TAILQ_REMOVE(&(parent->floating_head), con, floating_windows)
    pub fn floating_remove(&mut self, parent: ConId, con: ConId) {
        Self::dq_remove(&mut self.cons[parent].floating_head, con);
    }

    /// TAILQ_INSERT_HEAD(&(parent->nodes_head), con, nodes)
    pub fn nodes_insert_head(&mut self, parent: ConId, con: ConId) {
        self.cons[parent].nodes_head.push_front(con);
    }

    /// TAILQ_INSERT_TAIL(&(parent->nodes_head), con, nodes)
    pub fn nodes_insert_tail(&mut self, parent: ConId, con: ConId) {
        self.cons[parent].nodes_head.push_back(con);
    }

    /// TAILQ_INSERT_BEFORE(before, con, nodes) — inserts into before's
    /// parent's nodes_head.
    pub fn nodes_insert_before(&mut self, before: ConId, con: ConId) {
        let parent = self.cons[before].parent.expect("nodes_insert_before: before has no parent");
        let nodes = &mut self.cons[parent].nodes_head;
        let idx = nodes.iter().position(|&c| c == before).expect("nodes_insert_before: before not in parent's nodes_head");
        nodes.insert(idx, con);
    }

    /// TAILQ_INSERT_AFTER(&(parent->nodes_head), after, con, nodes)
    pub fn nodes_insert_after(&mut self, after: ConId, con: ConId) {
        let parent = self.cons[after].parent.expect("nodes_insert_after: after has no parent");
        let nodes = &mut self.cons[parent].nodes_head;
        let idx = nodes.iter().position(|&c| c == after).expect("nodes_insert_after: after not in parent's nodes_head");
        nodes.insert(idx + 1, con);
    }

    /// TAILQ_INSERT_HEAD(&(parent->focus_head), con, focused)
    pub fn focus_insert_head(&mut self, parent: ConId, con: ConId) {
        self.cons[parent].focus_head.push_front(con);
    }

    /// TAILQ_INSERT_TAIL(&(parent->focus_head), con, focused)
    pub fn focus_insert_tail(&mut self, parent: ConId, con: ConId) {
        self.cons[parent].focus_head.push_back(con);
    }

    /// TAILQ_INSERT_BEFORE(before, con, focused)
    pub fn focus_insert_before(&mut self, before: ConId, con: ConId) {
        let parent = self.cons[before].parent.expect("focus_insert_before: before has no parent");
        let focus = &mut self.cons[parent].focus_head;
        let idx = focus.iter().position(|&c| c == before).expect("focus_insert_before: before not in parent's focus_head");
        focus.insert(idx, con);
    }

    /// TAILQ_INSERT_AFTER(&(parent->focus_head), after, con, focused)
    pub fn focus_insert_after(&mut self, after: ConId, con: ConId) {
        let parent = self.cons[after].parent.expect("focus_insert_after: after has no parent");
        let focus = &mut self.cons[parent].focus_head;
        let idx = focus.iter().position(|&c| c == after).expect("focus_insert_after: after not in parent's focus_head");
        focus.insert(idx + 1, con);
    }

    /// TAILQ_INSERT_HEAD(&(parent->floating_head), con, floating_windows)
    pub fn floating_insert_head(&mut self, parent: ConId, con: ConId) {
        self.cons[parent].floating_head.push_front(con);
    }

    /// TAILQ_INSERT_TAIL(&(parent->floating_head), con, floating_windows)
    pub fn floating_insert_tail(&mut self, parent: ConId, con: ConId) {
        self.cons[parent].floating_head.push_back(con);
    }

    /// TAILQ_REPLACE(&(parent->nodes_head), old, new, nodes)
    pub fn nodes_replace(&mut self, parent: ConId, old: ConId, new: ConId) {
        let nodes = &mut self.cons[parent].nodes_head;
        let idx = nodes.iter().position(|&c| c == old).expect("nodes_replace: old not in nodes_head");
        nodes[idx] = new;
    }

    /// TAILQ_REPLACE(&(parent->focus_head), old, new, focused)
    pub fn focus_replace(&mut self, parent: ConId, old: ConId, new: ConId) {
        let focus = &mut self.cons[parent].focus_head;
        let idx = focus.iter().position(|&c| c == old).expect("focus_replace: old not in focus_head");
        focus[idx] = new;
    }

    /// TAILQ_SWAP of two members of the same (or two different) nodes lists,
    /// general form used by con_swap (see con.rs).
    pub fn swap_in_list(&mut self, head_parent_of_first: ConId, head_parent_of_second: ConId, first: ConId, second: ConId, list_is_nodes: bool) {
        // The macro in C computes both prev pointers up-front.
        let first_prev = if list_is_nodes { self.nodes_prev(first) } else { self.focus_prev(first) };
        let second_prev = if list_is_nodes { self.nodes_prev(second) } else { self.focus_prev(second) };

        if second_prev == Some(first) {
            // TAILQ_SWAP(first, second, head1, field)
            let (p, a, b) = (head_parent_of_first, first, second);
            self.list_swap_adjacent(p, a, b, list_is_nodes);
        } else if first_prev == Some(second) {
            // TAILQ_SWAP(second, first, head1, field)
            let (p, a, b) = (head_parent_of_first, second, first);
            self.list_swap_adjacent(p, a, b, list_is_nodes);
        } else {
            // remove both, then insert at each other's position
            if list_is_nodes {
                self.nodes_remove(head_parent_of_first, first);
                self.nodes_remove(head_parent_of_second, second);
                match second_prev {
                    None => self.nodes_insert_head(head_parent_of_second, first),
                    Some(sp) => self.nodes_insert_after(sp, first),
                }
                match first_prev {
                    None => self.nodes_insert_head(head_parent_of_first, second),
                    Some(fp) => self.nodes_insert_after(fp, second),
                }
            } else {
                self.focus_remove(head_parent_of_first, first);
                self.focus_remove(head_parent_of_second, second);
                match second_prev {
                    None => self.focus_insert_head(head_parent_of_second, first),
                    Some(sp) => self.focus_insert_after(sp, first),
                }
                match first_prev {
                    None => self.focus_insert_head(head_parent_of_first, second),
                    Some(fp) => self.focus_insert_after(fp, second),
                }
            }
        }
    }

    fn list_swap_adjacent(&mut self, parent: ConId, a: ConId, b: ConId, list_is_nodes: bool) {
        // a and b are adjacent; TAILQ_SWAP(a, b) exchanges them in place.
        if list_is_nodes {
            let nodes = &mut self.cons[parent].nodes_head;
            let ia = nodes.iter().position(|&c| c == a).expect("list_swap_adjacent: a missing");
            let ib = nodes.iter().position(|&c| c == b).expect("list_swap_adjacent: b missing");
            nodes.swap(ia, ib);
        } else {
            let focus = &mut self.cons[parent].focus_head;
            let ia = focus.iter().position(|&c| c == a).expect("list_swap_adjacent: a missing");
            let ib = focus.iter().position(|&c| c == b).expect("list_swap_adjacent: b missing");
            focus.swap(ia, ib);
        }
    }

    /// window_free(): remove a Window from the tree (C: src/manage.c window_free).
    pub fn window_remove(&mut self, window: WindowId) {
        if let Some(w) = self.windows.get(window) {
            self.window_index.remove(&w.id);
        }
        self.windows.remove(window);
    }
}

/// C: src/tree.c:27 `_create___i3`
fn _create___i3(t: &mut Tree, b: &mut dyn ConBackend) -> ConId {
    let croot = t.croot.expect("_create___i3: no croot");
    let __i3 = con_new(t, b, Some(croot), None);
    t.cons[__i3].name = Some("__i3".to_string());
    t.cons[__i3].r#type = ConType::CT_OUTPUT;
    t.cons[__i3].layout = Layout::L_OUTPUT;
    con_fix_percent(t, croot);
    b.x_set_name(t, __i3, "[i3 con] pseudo-output __i3");
    /* For retaining the correct position/size of a scratchpad window, the
     * dimensions of the real outputs should be multiples of the __i3
     * pseudo-output. Ensuring that is the job of scratchpad_fix_resolution()
     * which gets called after this function and after detecting all the
     * outputs (or whenever an output changes). */
    t.cons[__i3].rect.width = 1280;
    t.cons[__i3].rect.height = 1024;

    /* Add a content container. */
    let content = con_new(t, b, None, None);
    t.cons[content].r#type = ConType::CT_CON;
    t.cons[content].name = Some("content".to_string());
    t.cons[content].layout = Layout::L_SPLITH;

    b.x_set_name(t, content, "[i3 con] content __i3");
    crate::con::con_attach(t, b, content, __i3, false);

    /* Attach the __i3_scratch workspace. */
    let ws = con_new(t, b, None, None);
    t.cons[ws].r#type = ConType::CT_WORKSPACE;
    t.cons[ws].num = -1;
    t.cons[ws].name = Some("__i3_scratch".to_string());
    t.cons[ws].layout = Layout::L_SPLITH;
    crate::con::con_attach(t, b, ws, content, false);
    b.x_set_name(t, ws, "[i3 con] workspace __i3_scratch");
    t.cons[ws].fullscreen_mode = FullscreenMode::CF_OUTPUT;

    __i3
}

/// C: src/tree.c:56 `tree_restore`
///
/// Loads tree from 'path' (used for in-place restarts).
// The C initialization "bool result = false;" (src/tree.c:64) is kept 1:1;
// every path either returns early or assigns result before reading it.
#[allow(unused_assignments)]
pub fn tree_restore(t: &mut Tree, b: &mut dyn ConBackend, path: &str, geometry: Rect) -> bool {
    let mut result = false;

    let buf = match b.slurp(path) {
        None => return false,
        Some(buf) => buf,
    };

    /* TODO: refactor the following */
    let croot = con_new(t, b, None, None);
    t.cons[croot].rect = geometry;
    t.focused = Some(croot);
    t.croot = Some(croot);

    b.tree_append_json(t, croot, &buf);

    let new_root = t.cons[croot].nodes_head.front().copied();
    let new_root = match new_root {
        None => {
            /* tree_append_json failed. Continuing here would segfault. */
            return false;
        }
        Some(r) => r,
    };
    t.croot = Some(new_root);
    let out = t.cons[new_root].nodes_head.front().copied().expect("tree_restore: no output below root");
    let _ws = t.cons[out].nodes_head.front().copied();

    /* For in-place restarting into v4.2, we need to make sure the new
     * pseudo-output __i3 is present. */
    if t.cons[out].name.as_deref() != Some("__i3") {
        let __i3 = _create___i3(t, b);
        /* Ensure that it is the first output, other places in the code make
         * that assumption. */
        let parent = t.cons[__i3].parent.expect("tree_restore: __i3 has no parent");
        t.nodes_remove(parent, __i3);
        t.nodes_insert_head(parent, __i3);
    }

    b.restore_open_placeholder_windows(t, new_root);
    result = true;

    result
}

/// C: src/tree.c:98 `tree_init`
///
/// Initializes the tree by creating the root node. The CT_OUTPUT Cons below
/// the root node are created in randr.c for each Output.
pub fn tree_init(t: &mut Tree, b: &mut dyn ConBackend, geometry: Rect) {
    let croot = con_new(t, b, None, None);
    t.croot = Some(croot);
    t.cons[croot].name = Some("root".to_string());
    t.cons[croot].r#type = ConType::CT_ROOT;
    t.cons[croot].layout = Layout::L_SPLITH;
    t.cons[croot].rect = geometry;

    _create___i3(t, b);
}

/// C: src/tree.c:126 `tree_open_con`
///
/// Opens an empty container in the current container.
pub fn tree_open_con(t: &mut Tree, b: &mut dyn ConBackend, con: Option<ConId>, window: Option<WindowId>) -> ConId {
    let con = match con {
        Some(con) => con,
        None => {
            let focused = t.focused.expect("tree_open_con: no focused con");
            /* every focusable Con has a parent (outputs have parent root) */
            let mut con = t.cons[focused].parent.expect("tree_open_con: focused has no parent");
            /* If the parent is an output, we are on a workspace. In this case,
             * the new container needs to be opened as a leaf of the workspace. */
            let grandparent_type = t.cons[t.cons[con].parent.expect("tree_open_con: no grandparent")].r#type;
            if grandparent_type == ConType::CT_OUTPUT && t.cons[con].r#type != ConType::CT_DOCKAREA {
                con = focused;
            }

            /* If the currently focused container is a floating container, we
             * attach the new container to the currently focused spot in its
             * workspace. */
            if t.cons[con].r#type == ConType::CT_FLOATING_CON {
                let parent = t.cons[con].parent.expect("tree_open_con: floating has no parent");
                con = con_descend_tiling_focused(t, parent);
                if t.cons[con].r#type != ConType::CT_WORKSPACE {
                    con = t.cons[con].parent.expect("tree_open_con: no parent");
                }
            }
            con
        }
    };

    /* 3. create the container and attach it to its parent */
    let new = con_new(t, b, Some(con), window);
    t.cons[new].layout = Layout::L_SPLITH;

    /* 4: re-calculate child->percent for each child */
    con_fix_percent(t, con);

    new
}

/// C: src/tree.c:175 `tree_close_internal`
///
/// Closes the given container including all children.
/// Returns true if the container was killed or false if just WM_DELETE was
/// sent and the window is expected to kill itself.
///
/// The dont_kill_parent flag is specified when the function calls itself
/// recursively while deleting a containers children.
pub fn tree_close_internal(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, kill_window: crate::data::KillWindow, dont_kill_parent: bool) -> bool {
    // C: src/tree.c:202 DLOG("closing %p, kill_window = %d\n", con, kill_window);
    crate::log::dlog_at(
        file!(),
        "tree_close_internal",
        line!(),
        &format!(
            "closing {} , kill_window = {}\n",
            crate::manage::con_ptr(con),
            kill_window as i32
        ),
    );

    let parent = t.cons[con].parent;

    /* remove the urgency hint of the workspace (if set) */
    if t.cons[con].urgent {
        con_set_urgency(t, b, con, false);
        con_update_parents_urgency(t, con);
        if let Some(ws) = con_get_workspace(t, con) {
            workspace_update_urgent_flag(t, b, ws);
        }
    }

    let mut abort_kill = false;
    /* We cannot use TAILQ_FOREACH because the children get deleted
     * in their parent's nodes_head (the whole child list is snapshotted;
     * the recursion only ever removes entries from it). */
    let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in children {
        // C: src/tree.c:208 DLOG("killing child=%p\n", child);
        crate::log::dlog_at(
            file!(),
            "tree_close_internal",
            line!(),
            &format!("killing child={}\n", crate::manage::con_ptr(child)),
        );
        if !tree_close_internal(t, b, child, kill_window, true) {
            abort_kill = true;
        }
    }

    if abort_kill {
        return false;
    }

    if let Some(window) = t.cons[con].window {
        if kill_window != crate::data::KillWindow::DONT_KILL_WINDOW {
            b.x_window_kill(t, t.windows[window].id, kill_window);
            return false;
        }
        /* Ignore any further events by clearing the event mask,
         * unmap the window, then reparent it to the root window. */
        let (x, y) = (t.cons[con].rect.x, t.cons[con].rect.y);
        b.x_reparent_window_to_root(t, t.windows[window].id, x, y);

        /* We are no longer handling this window, thus set WM_STATE to
         * WM_STATE_WITHDRAWN (see ICCCM 4.1.3.1) */
        b.x_set_wm_state_withdrawn(t, t.windows[window].id);

        /* Remove the window from the save set. All windows in the save set
         * will be mapped when i3 closes its connection (e.g. when
         * restarting). This is not what we want, since some apps keep
         * unmapped windows around and don't expect them to suddenly be
         * mapped. See https://bugs.i3wm.org/1617 */
        b.x_change_save_set(t, t.windows[window].id);

        /* Stop receiving ShapeNotify events. */
        b.x_shape_select_input(t, t.windows[window].id, false);

        b.ipc_send_window_event(t, "close", con);
        t.window_remove(window);
        t.cons[con].window = None;
    }

    let ws = con_get_workspace(t, con);

    /* Figure out which container to focus next before detaching 'con'. */
    let next = if t.focused == Some(con) { Some(con_next_focused(t, con)) } else { None };

    /* Detach the container so that it will not be rendered anymore. */
    con_detach(t, con);

    /* disable urgency timer, if needed */
    if t.cons[con].urgency_timer.is_some() {
        if let Some(ws) = ws {
            workspace_update_urgent_flag(t, b, ws);
        }
        t.cons[con].urgency_timer = None;
    }

    if t.cons[con].r#type != ConType::CT_FLOATING_CON {
        /* If the container is *not* floating, we might need to re-distribute
         * percentage values for the resized containers. */
        let parent = parent.expect("tree_close_internal: no parent");
        con_fix_percent(t, parent);
    }

    /* Render the tree so that the surrounding containers take up the space
     * which 'con' does no longer occupy. If we don't render here, there will
     * be a gap in our containers and that could trigger an EnterNotify for an
     * underlying container, see ticket #660.
     *
     * Rendering has to be avoided when dont_kill_parent is set (when
     * tree_close_internal calls itself recursively) because the tree is in a
     * non-renderable state during that time. */
    if !dont_kill_parent {
        tree_render(t, b);
    }

    /* kill the X11 part of this container */
    b.x_con_kill(t, con);

    if ws == Some(con) {
        b.ewmh_update_desktop_properties(t);
    }

    crate::con::con_free(t, con);

    if let Some(next) = next {
        con_activate(t, b, next);
    }

    /* check if the parent container is empty now and close it */
    if !dont_kill_parent {
        let parent = parent.expect("tree_close_internal: no parent");
        con_on_remove_child(t, b, parent);
    }

    true
}

/// C: src/tree.c:313 `tree_split`
///
/// Splits (horizontally or vertically) the given container by creating a new
/// container which contains the old one and the future ones.
pub fn tree_split(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, orientation: crate::data::Orientation) {
    if con_is_floating(t, con) {
        return;
    }

    let mut con = con;

    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        if con_num_children(t, con) < 2 {
            if con_num_children(t, con) == 0 {
                t.cons[con].workspace_layout = Layout::L_DEFAULT;
            }
            t.cons[con].layout = if orientation == crate::data::Orientation::HORIZ { Layout::L_SPLITH } else { Layout::L_SPLITV };
            return;
        } else {
            /* if there is more than one container on the workspace
             * move them into a new container and handle this instead */
            con = crate::workspace::workspace_encapsulate(t, b, con).expect("tree_split: workspace_encapsulate failed");
        }
    }

    let parent = t.cons[con].parent.expect("tree_split: no parent");

    /* Force re-rendering to make the indicator border visible. */
    crate::con::con_force_split_parents_redraw(t, con);

    /* if we are in a container whose parent contains only one
     * child (its split functionality is unused so far), we just change the
     * orientation (more intuitive than splitting again) */
    if con_num_children(t, parent) == 1
        && (t.cons[parent].layout == Layout::L_SPLITH || t.cons[parent].layout == Layout::L_SPLITV)
    {
        t.cons[parent].layout = if orientation == crate::data::Orientation::HORIZ { Layout::L_SPLITH } else { Layout::L_SPLITV };
        return;
    }

    /* 2: replace it with a new Con */
    let new = con_new(t, b, None, None);
    t.nodes_replace(parent, con, new);
    t.focus_replace(parent, con, new);
    t.cons[new].parent = Some(parent);
    t.cons[new].layout = if orientation == crate::data::Orientation::HORIZ { Layout::L_SPLITH } else { Layout::L_SPLITV };

    /* 3: swap 'percent' (resize factor) */
    t.cons[new].percent = t.cons[con].percent;
    t.cons[con].percent = 0.0;

    /* 4: add it as a child to the new Con */
    crate::con::con_attach(t, b, con, new, false);
}

/// C: src/tree.c:375 `level_up`
///
/// Moves focus one level up. Returns true if focus changed.
pub fn level_up(t: &mut Tree, b: &mut dyn ConBackend) -> bool {
    let focused = t.focused.expect("level_up: no focused con");
    let parent = t.cons[focused].parent.expect("level_up: no parent");
    /* Skip over floating containers and go directly to the grandparent
     * (which should always be a workspace) */
    if t.cons[parent].r#type == ConType::CT_FLOATING_CON {
        let grandparent = t.cons[parent].parent.expect("level_up: no grandparent");
        con_activate(t, b, grandparent);
        return true;
    }

    /* We can focus up to the workspace, but not any higher in the tree */
    if (t.cons[parent].r#type != ConType::CT_CON && t.cons[parent].r#type != ConType::CT_WORKSPACE)
        || t.cons[focused].r#type == ConType::CT_WORKSPACE
    {
        return false;
    }
    con_activate(t, b, parent);
    true
}

/// C: src/tree.c:394 `level_down`
///
/// Moves focus one level down. Returns true if focus changed.
pub fn level_down(t: &mut Tree, b: &mut dyn ConBackend) -> bool {
    let focused = t.focused.expect("level_down: no focused con");
    /* Go down the focus stack of the current node */
    let next = t.cons[focused].focus_head.front().copied();
    match next {
        None => false,
        Some(mut n) => {
            if t.cons[n].r#type == ConType::CT_FLOATING_CON {
                /* Floating cons shouldn't be directly focused; try immediately
                 * going to the grandchild of the focused con. */
                match t.cons[n].focus_head.front().copied() {
                    None => return false,
                    Some(child) => n = child,
                }
            }
            con_activate(t, b, n);
            true
        }
    }
}

/// C: src/tree.c:432 `mark_unmapped`
fn mark_unmapped(t: &mut Tree, con: ConId) {
    t.cons[con].mapped = false;
    let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for current in children {
        mark_unmapped(t, current);
    }
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        /* We need to call mark_unmapped on floating nodes as well since we can
         * make containers floating. */
        let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
        for current in floating {
            mark_unmapped(t, current);
        }
    }
}

/// Whether a render+push is still owed to X11.
///
/// C: `cmd_output->needs_tree_render` (src/commands.c) plus the fact that
/// every direct caller of tree_render() is synchronous. The port's event loop
/// is where the push happens (crates/bins/i3/src/server.rs), so the "tree is
/// dirty" state has to survive until then: every place that mutates the tree
/// without rendering immediately calls request_tree_render(), and tree_render()
/// clears the request because it renders right away.
static NEEDS_TREE_RENDER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Marks the tree as dirty (C: `cmd_output->needs_tree_render = true`).
///
/// The next iteration of the event loop renders and pushes the tree.
pub fn request_tree_render() {
    NEEDS_TREE_RENDER.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// C: the main loop's "if (result->needs_tree_render) tree_render();"
/// (src/bindings.c:879, src/ipc.c:218) — consumes the pending request.
pub fn take_tree_render_request() -> bool {
    NEEDS_TREE_RENDER.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Reads the pending request without consuming it (tests / diagnostics).
pub fn tree_render_requested() -> bool {
    NEEDS_TREE_RENDER.load(std::sync::atomic::Ordering::Relaxed)
}

/// C: src/tree.c:449 `tree_render`
///
/// Renders the tree, that is rendering all outputs using render_con() and
/// pushing the changes to X11 using x_push_changes().
pub fn tree_render(t: &mut Tree, b: &mut dyn ConBackend) {
    // Rendering right now satisfies any pending request (in C the caller
    // clears needs_tree_render when it calls tree_render()).
    take_tree_render_request();

    let croot = match t.croot {
        None => return,
        Some(croot) => croot,
    };

    /* Reset map state for all nodes in tree */
    mark_unmapped(t, croot);
    t.cons[croot].mapped = true;

    render_con(t, b, croot);

    b.x_push_changes(t, croot);
}

/// C: src/tree.c:466 `get_tree_next_workspace`
fn get_tree_next_workspace(t: &Tree, con: ConId, direction: Direction) -> Option<ConId> {
    if con_get_fullscreen_con(t, con, FullscreenMode::CF_GLOBAL).is_some() {
        return None;
    }

    // Use the center of the container instead of the left/top edges, to make
    // this work with negative gaps. See https://github.com/i3/i3/issues/5293
    let rect = t.cons[con].rect;
    let x = rect.x.wrapping_add(rect.width / 2);
    let y = rect.y.wrapping_add(rect.height / 2);
    let current_output = get_output_containing(t, x, y)?;
    let next_output = get_output_next(t, direction, current_output, OutputCloseFar::CLOSEST_OUTPUT)?;

    /* Find visible workspace on next output */
    let content = output_get_content(t, t.outputs[next_output].con?)?;
    for &child in &t.cons[content].nodes_head {
        if workspace_is_visible(t, child) {
            return Some(child);
        }
    }
    None
}

/// C: src/tree.c:490 `get_tree_next`
///
/// Returns the next / previous container to focus in the given direction.
/// Does not modify focus and ensures focus restrictions for fullscreen
/// containers are respected.
fn get_tree_next(t: &Tree, con: ConId, direction: Direction) -> Option<ConId> {
    use crate::data::{FocusWrapping, Orientation, Position};

    let previous = position_from_direction(direction) == Position::BEFORE;
    let orientation = orientation_from_direction(direction);

    let mut first_wrap: Option<ConId> = None;

    let mut con = con;

    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        /* Special case for FOCUS_WRAPPING_WORKSPACE: allow the focus to leave
         * the workspace only when a workspace is selected. (goto
         * handle_workspace) */
        return get_tree_next_workspace(t, con, direction).or(first_wrap);
    }

    while t.cons[con].r#type != ConType::CT_WORKSPACE {
        if t.cons[con].fullscreen_mode == FullscreenMode::CF_OUTPUT {
            /* We've reached a fullscreen container. Directional focus should
             * now operate on the workspace level. */
            con = con_get_workspace(t, con)?;
            break;
        } else if t.cons[con].fullscreen_mode == FullscreenMode::CF_GLOBAL {
            /* Focus changes should happen only inside the children of a global
             * fullscreen container. */
            return first_wrap;
        }

        let parent = t.cons[con].parent?;
        if t.cons[con].r#type == ConType::CT_FLOATING_CON {
            if orientation != Orientation::HORIZ {
                /* up/down does not change floating containers */
                return None;
            }

            /* left/right focuses the previous/next floating container */
            let mut next = if previous { t.floating_prev(con) } else { t.floating_next(con) };
            /* If there is no next/previous container, wrap */
            if next.is_none() {
                next = if previous {
                    t.cons[parent].floating_head.back().copied()
                } else {
                    t.cons[parent].floating_head.front().copied()
                };
            }
            /* Our parent does not list us in floating heads? (C asserts.) */
            return next;
        }

        if con_num_children(t, parent) > 1 && con_orientation(t, parent) == orientation {
            let next = if previous { t.nodes_prev(con) } else { t.nodes_next(con) };
            if let Some(n) = next {
                if con_fullscreen_permits_focusing(t, Some(n)) {
                    return Some(n);
                }
            }

            let wrap = if previous {
                t.cons[parent].nodes_head.back().copied()
            } else {
                t.cons[parent].nodes_head.front().copied()
            };
            match t.config.focus_wrapping {
                FocusWrapping::FOCUS_WRAPPING_OFF => {}
                FocusWrapping::FOCUS_WRAPPING_WORKSPACE | FocusWrapping::FOCUS_WRAPPING_ON => {
                    if first_wrap.is_none() {
                        if let Some(w) = wrap {
                            if con_fullscreen_permits_focusing(t, Some(w)) {
                                first_wrap = Some(w);
                            }
                        }
                    }
                }
                FocusWrapping::FOCUS_WRAPPING_FORCE => {
                    /* 'force' should always return to ensure focus doesn't
                     * leave the parent. */
                    if next.is_some() {
                        return None; /* blocked by fullscreen */
                    }
                    return match wrap {
                        Some(w) if con_fullscreen_permits_focusing(t, Some(w)) => Some(w),
                        _ => None,
                    };
                }
            }
        }

        con = parent;
    }

    if t.config.focus_wrapping == FocusWrapping::FOCUS_WRAPPING_WORKSPACE {
        return first_wrap;
    }

    // handle_workspace:
    let workspace = get_tree_next_workspace(t, con, direction);
    workspace.or(first_wrap)
}

/// C: src/tree.c:583 `tree_next`
///
/// Changes focus in the given direction.
pub fn tree_next(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, direction: Direction) {
    let next = match get_tree_next(t, con, direction) {
        None => return,
        Some(next) => next,
    };
    if t.cons[next].r#type == ConType::CT_WORKSPACE {
        /* Show next workspace and focus appropriate container if possible. */
        /* Use descend_focused first to give higher priority to floating or
         * tiling fullscreen containers. */
        let mut focus = con_descend_focused(t, next);
        if t.cons[focus].fullscreen_mode == FullscreenMode::CF_NONE {
            let focus_tiling = con_descend_tiling_focused(t, next);
            /* If descend_tiling returned a workspace then focus is either a
             * floating container or the same workspace. */
            if focus_tiling != next {
                focus = focus_tiling;
            }
        }

        workspace_show(t, b, next);
        con_activate(t, b, focus);
        // C: src/tree.c:614 x_set_warp_to(&(focus->rect));
        b.x_set_warp_to(t, Some(focus));
        return;
    } else if t.cons[next].r#type == ConType::CT_FLOATING_CON {
        /* Raise the floating window on top of other windows preserving relative
         * stack order */
        let parent = t.cons[next].parent.expect("tree_next: floating has no parent");
        while t.cons[parent].floating_head.back() != Some(&next) {
            let last = *t.cons[parent].floating_head.back().expect("tree_next: floating_head empty");
            t.floating_remove(parent, last);
            t.floating_insert_head(parent, last);
        }
    }

    let ws = con_get_workspace(t, next).expect("tree_next: no workspace");
    workspace_show(t, b, ws);
    con_activate(t, b, con_descend_focused(t, next));
}

/// C: src/tree.c:628 `get_tree_next_sibling`
///
/// Get the previous / next sibling.
pub fn get_tree_next_sibling(t: &Tree, con: ConId, direction: crate::data::Position) -> Option<ConId> {
    let to_focus = if direction == crate::data::Position::BEFORE {
        t.nodes_prev(con)
    } else {
        t.nodes_next(con)
    };
    if let Some(to_focus) = to_focus {
        if con_fullscreen_permits_focusing(t, Some(to_focus)) {
            return Some(to_focus);
        }
    }
    None
}

/// C: src/tree.c:641 `tree_flatten`
///
/// tree_flatten() removes pairs of redundant split containers, e.g.:
///       [workspace, horizontal]
///   [v-split]           [child3]
///   [h-split]
/// [child1] [child2]
/// In this example, the v-split and h-split container are redundant.
pub fn tree_flatten(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    let parent = t.cons[con].parent;

    /* We only consider normal containers without windows */
    let mut can_flatten = false;
    if let Some(parent) = parent {
        if t.cons[con].r#type == ConType::CT_CON
            && t.cons[parent].layout != Layout::L_OUTPUT /* con == "content" */
            && t.cons[con].window.is_none()
        {
            /* Ensure it got only one child */
            let child = t.cons[con].nodes_head.front().copied();
            if let Some(child) = child {
                if t.nodes_next(child).is_none() {
                    /* The child must have a different orientation than the con but the same as
                     * the con's parent to be redundant */
                    if con_is_split(t, con)
                        && con_is_split(t, child)
                        && (t.cons[con].layout == Layout::L_SPLITH || t.cons[con].layout == Layout::L_SPLITV)
                        && (t.cons[child].layout == Layout::L_SPLITH || t.cons[child].layout == Layout::L_SPLITV)
                        && con_orientation(t, con) != con_orientation(t, child)
                        && con_orientation(t, child) == con_orientation(t, parent)
                    {
                        can_flatten = true;
                    }
                }
            }
        }
    }

    if can_flatten {
        let parent = parent.expect("tree_flatten: no parent");
        let child = t.cons[con].nodes_head.front().copied().expect("tree_flatten: no child");

        /* 1: save focus */
        let focus_next = t.cons[child].focus_head.front().copied();

        /* 2: re-attach the children to the parent before con */
        while let Some(current) = t.cons[child].nodes_head.front().copied() {
            con_detach(t, current);
            /* We don't use con_attach() here because for a CT_CON, the special
             * case handling of con_attach() does not trigger. So all it would do
             * is calling TAILQ_INSERT_AFTER, but with the wrong container. So we
             * directly use the TAILQ macros. */
            t.cons[current].parent = Some(parent);
            t.nodes_insert_before(con, current);
            t.focus_insert_tail(parent, current);
            let percent = t.cons[con].percent;
            t.cons[current].percent = percent;
        }

        /* 3: restore focus, if con was focused */
        if let Some(focus_next) = focus_next {
            if t.cons[parent].focus_head.front() == Some(&con) {
                t.focus_remove(parent, focus_next);
                t.focus_insert_head(parent, focus_next);
            }
        }

        /* 4: close the redundant cons */
        tree_close_internal(t, b, con, crate::data::KillWindow::DONT_KILL_WINDOW, true);

        /* Well, we got to abort the recursion here because we destroyed the
         * container. However, if tree_flatten() is called sufficiently often,
         * there can't be the situation of having two pairs of redundant containers
         * at once. Therefore, we can safely abort the recursion on this level
         * after flattening. */
        return;
    }

    /* recurse: (We cannot use normal foreach here because tree_flatten might
     * close the current container.) */
    let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for current in children {
        tree_flatten(t, b, current);
    }

    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for current in floating {
        tree_flatten(t, b, current);
    }
}
