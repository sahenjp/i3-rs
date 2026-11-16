//! randr.c: RandR output handling + xinerama.c (1:1 port of the X11 side).
//!
//! C: src/randr.c, src/xinerama.c. The pure-logic helpers (get_output_by_name,
//! get_output_containing, get_output_next, ...) already live in output.rs; this
//! module ports the parts that query the X server: randr_init,
//! randr_query_outputs (1.5 preferred / 1.4 fallback), output_init_con,
//! init_ws_for_output, output_change_mode, randr_disable_output,
//! fallback_to_root_output and xinerama_init.
//!
//! The RandR requests go through the XConn abstraction (backend.rs) so that the
//! two-stage query flow can be tested with the canned RandR replies of
//! RecordingConn; RustXConn serves the same calls from x11rb's randr module.

// Only the test module below builds fake MonitorInfo values.
#[cfg(test)]
use crate::backend::MonitorInfo;
use crate::backend::{
    ConBackend, OutputInfo, XConn, XCB_NONE, XCB_RANDR_NOTIFY_MASK_CRTC_CHANGE,
    XCB_RANDR_NOTIFY_MASK_OUTPUT_CHANGE, XCB_RANDR_NOTIFY_MASK_OUTPUT_PROPERTY,
    XCB_RANDR_NOTIFY_MASK_SCREEN_CHANGE,
};
use crate::con::{
    con_attach, con_detach, con_fix_percent, con_focus, con_get_workspace,
    con_is_internal, con_new, con_num_children,
};
use crate::data::{
    output_name, ConId, ConType, FullscreenMode, KillWindow, Layout, Match, MatchDock,
    MatchInsertWhere, Orientation, Output, Rect,
};
use crate::floating::floating_fix_coordinates;
use crate::log::{debuglog, errorlog};
use crate::output::{get_output_by_name, output_get_content, output_primary_name, OutputId};
use crate::tree::{tree_close_internal, tree_render, Tree};
use crate::workspace::{
    create_workspace_on_output, get_assigned_output, output_triggers_assignment,
    workspace_move_to_output, workspace_show, workspace_show_by_name,
};

/// C: src/randr.c:26 static bool has_randr_1_5 = false;
pub struct RandrState {
    /// C: randr_base (the RandR extension event base; -1 when unavailable)
    pub randr_base: i32,
    pub has_randr_1_5: bool,
    /// C: src/randr.c:25 static Output *root_output; (index into Tree::outputs)
    pub root_output: Option<usize>,
}

impl Default for RandrState {
    fn default() -> Self {
        RandrState { randr_base: -1, has_randr_1_5: false, root_output: None }
    }
}

/// C: src/util.c:48 update_if_necessary
fn update_if_necessary(dst: &mut u32, src: u32) -> bool {
    if *dst == src {
        return false;
    }
    *dst = src;
    true
}

/// The swallow criterion of a dockarea.
///
/// C: src/randr.c:390-396 / :419-425 — match_init(match); match->dock = dock;
/// match->insert_where = M_BELOW. match_init() is essential: it sets
/// window_type = UINT32_MAX, urgent = U_DONTCHECK and window_mode = WM_ANY, so
/// match_matches_window() only tests the dock field.
fn dockarea_match(dock: MatchDock) -> Match {
    Match {
        dock,
        insert_where: MatchInsertWhere::M_BELOW,
        // C: src/match.c:60-70 match_init
        urgent: crate::data::MatchUrgency::U_DONTCHECK,
        window_mode: crate::data::MatchWindowMode::WM_ANY,
        window_type: u32::MAX,
        ..Default::default()
    }
}

/// C: src/randr.c:34 get_output_by_id (index into Tree::outputs)
pub fn get_output_by_id(t: &Tree, id: u32) -> Option<OutputId> {
    for (idx, output) in t.outputs.iter().enumerate() {
        if output.id == id {
            return Some(idx);
        }
    }
    None
}

/// C: src/randr.c:105 any_randr_output_active
pub fn any_randr_output_active(t: &Tree) -> bool {
    t.outputs.iter().any(|o| o.active)
}

/// C: src/randr.c:313 create_root_output
///
/// Creates an output covering the root window (inactive until no other output
/// is active).
fn create_root_output(x: &mut dyn XConn) -> Output {
    let screen = x.root_screen();
    let mut names_head = std::collections::VecDeque::new();
    names_head.push_back(output_name { name: "xroot-0".to_string() });
    Output {
        id: 0,
        active: false,
        changed: false,
        to_be_disabled: false,
        primary: false,
        names_head,
        con: None,
        rect: Rect::new(0, 0, u32::from(screen.width_in_pixels), u32::from(screen.height_in_pixels)),
    }
}

/// C: src/randr.c:347 output_init_con
///
/// Creates the output container (CT_OUTPUT) for the given output and links it,
/// including the top/bottom dockareas and the content container.
pub fn output_init_con(t: &mut Tree, b: &mut dyn ConBackend, output_idx: OutputId) -> ConId {
    let name = output_primary_name(t, output_idx);
    let croot = t.croot.expect("output_init_con: no croot");

    // C: search for a Con with that name directly below the root node (there
    // might be one from a restored layout).
    let mut con: Option<ConId> = None;
    let mut reused = false;
    let children: Vec<ConId> = t.cons[croot].nodes_head.iter().copied().collect();
    for current in children {
        if t.cons[current].name.as_deref() != Some(name.as_str()) {
            continue;
        }
        con = Some(current);
        reused = true;
        debuglog(&format!("Using existing con {:?} / {}", current, name));
        break;
    }

    let con = match con {
        Some(con) => con,
        None => {
            let con = con_new(t, b, Some(croot), None);
            t.cons[con].name = Some(name.clone());
            t.cons[con].r#type = ConType::CT_OUTPUT;
            t.cons[con].layout = Layout::L_OUTPUT;
            con_fix_percent(t, croot);
            con
        }
    };

    // C: con->rect = output->rect; output->con = con;
    let rect = t.outputs[output_idx].rect;
    t.cons[con].rect = rect;
    t.outputs[output_idx].con = Some(con);

    // C: x_set_name(con, "[i3 con] output %s")
    b.x_set_name(t, con, &format!("[i3 con] output {name}"));

    if reused {
        return con;
    }

    // C: topdock = con_new(NULL, NULL); type CT_DOCKAREA; layout L_DOCKAREA
    //    match->dock = M_DOCK_TOP; match->insert_where = M_BELOW;
    let topdock = con_new(t, b, None, None);
    t.cons[topdock].r#type = ConType::CT_DOCKAREA;
    t.cons[topdock].layout = Layout::L_DOCKAREA;
    // C: src/randr.c:390-396 — Match *match = smalloc(); match_init(match);
    // match->dock = M_DOCK_TOP; match->insert_where = M_BELOW;
    //
    // match_init() matters: it sets window_type to UINT32_MAX ("not set"), so
    // that match_matches_window() does not compare it against the window. With
    // a zero-initialised Match a dock window (window_type =
    // _NET_WM_WINDOW_TYPE_DOCK) never matches its dockarea and ends up tiled.
    t.cons[topdock].swallow_head.push_back(dockarea_match(MatchDock::M_DOCK_TOP));
    t.cons[topdock].name = Some("topdock".to_string());
    b.x_set_name(t, topdock, &format!("[i3 con] top dockarea {name}"));
    con_attach(t, b, topdock, con, false);

    // C: content = con_new(NULL, NULL); type CT_CON; layout L_SPLITH
    let content = con_new(t, b, None, None);
    t.cons[content].r#type = ConType::CT_CON;
    t.cons[content].layout = Layout::L_SPLITH;
    t.cons[content].name = Some("content".to_string());
    b.x_set_name(t, content, &format!("[i3 con] content {name}"));
    con_attach(t, b, content, con, false);

    // C: bottomdock = con_new(NULL, NULL); match->dock = M_DOCK_BOTTOM
    let bottomdock = con_new(t, b, None, None);
    t.cons[bottomdock].r#type = ConType::CT_DOCKAREA;
    t.cons[bottomdock].layout = Layout::L_DOCKAREA;
    t.cons[bottomdock].swallow_head.push_back(dockarea_match(MatchDock::M_DOCK_BOTTOM));
    t.cons[bottomdock].name = Some("bottomdock".to_string());
    b.x_set_name(t, bottomdock, &format!("[i3 con] bottom dockarea {name}"));
    con_attach(t, b, bottomdock, con, false);

    // C: change the focus to the content container
    //    (TAILQ_REMOVE(&focus_head, content); TAILQ_INSERT_HEAD(&focus_head, content))
    let mut focus: Vec<ConId> = t.cons[con].focus_head.iter().copied().collect();
    focus.retain(|c| *c != content);
    t.cons[con].focus_head.clear();
    t.cons[con].focus_head.push_back(content);
    for child in focus {
        t.cons[con].focus_head.push_back(child);
    }

    con
}

/// C: src/randr.c:456 init_ws_for_output
///
/// Initializes at least one workspace for this output: move the assigned
/// workspaces, then create the first assigned workspace, then the first free
/// one.
pub fn init_ws_for_output(t: &mut Tree, b: &mut dyn ConBackend, output_idx: OutputId) {
    let output_con = t.outputs[output_idx].con.expect("init_ws_for_output: output has no con");
    let content = output_get_content(t, output_con).expect("init_ws_for_output: no content container");
    let previous_focus = t.focused.and_then(|focused| con_get_workspace(t, focused));

    // C: iterate over all_cons and move the workspaces assigned to this output.
    let all_cons: Vec<ConId> = t.all_cons.iter().copied().collect();
    for workspace in all_cons {
        if t.cons[workspace].r#type != ConType::CT_WORKSPACE || con_is_internal(t, workspace) {
            continue;
        }

        let ws_name = t.cons[workspace].name.clone().unwrap_or_default();
        let ws_num = t.cons[workspace].num as i64;
        let workspace_out = get_assigned_output(t, Some(ws_name.as_str()), ws_num);

        if workspace_out != Some(output_con) {
            continue;
        }

        // C: content->rect = output->con->rect (the content is not yet rendered)
        t.cons[content].rect = t.cons[output_con].rect;
        workspace_move_to_output(t, b, workspace, output_idx);
    }

    // C: temporarily set the focused container, might not be initialized yet.
    t.focused = Some(content);

    // C: if a workspace exists, we are done now
    if t.cons[content].nodes_head.front().is_some() {
        // C: ensure that one of the workspaces is actually visible (in
        // fullscreen mode); GREP_FIRST(visible, content, child->fullscreen_mode == CF_OUTPUT)
        let visible = t
            .cons[content]
            .nodes_head
            .iter()
            .copied()
            .find(|child| t.cons[*child].fullscreen_mode == FullscreenMode::CF_OUTPUT);
        let visible = match visible {
            Some(visible) => visible,
            None => *t
                .cons[content]
                .nodes_head
                .front()
                .expect("init_ws_for_output: no workspace"),
        };
        workspace_show(t, b, visible);
    } else {
        // C: otherwise, we create the first assigned ws for this output
        let assignments: Vec<crate::data::Workspace_Assignment> =
            t.ws_assignments.iter().cloned().collect();
        let mut created = false;
        for assignment in assignments {
            if !output_triggers_assignment(t, output_idx, &assignment) {
                continue;
            }

            debuglog(&format!(
                "Initializing first assigned workspace \"{}\" for output \"{}\"",
                assignment.name,
                output_primary_name(t, output_idx)
            ));
            workspace_show_by_name(t, b, &assignment.name);
            created = true;
            break;
        }

        if !created {
            // C: if there is still no workspace, we create the first free workspace
            let ws = create_workspace_on_output(t, b, output_idx, content);
            workspace_show(t, b, ws);
        }
    }

    // C: restore_focus: if (previous_focus) workspace_show(previous_focus);
    if let Some(previous) = previous_focus {
        if t.cons.get(previous).is_some() {
            workspace_show(t, b, previous);
        }
    }
}

/// C: src/randr.c:545 output_change_mode
///
/// Reconfigures the workspaces after the mode of an output changed.
fn output_change_mode(t: &mut Tree, output_idx: OutputId) {
    let con = t.outputs[output_idx].con.expect("output_change_mode: output has no con");
    debuglog("Output mode changed, updating rect");
    t.cons[con].rect = t.outputs[output_idx].rect;

    let content = output_get_content(t, con).expect("output_change_mode: no content container");
    let workspaces: Vec<ConId> = t.cons[content].nodes_head.iter().copied().collect();

    // C: fix the position of all floating windows on this output
    for workspace in &workspaces {
        let floats: Vec<ConId> = t.cons[*workspace].floating_head.iter().copied().collect();
        let ws_rect = t.cons[*workspace].rect;
        let con_rect = t.cons[con].rect;
        for child in floats {
            floating_fix_coordinates(t, child, &ws_rect, &con_rect);
        }
    }

    // C: if (config.default_orientation == NO_ORIENTATION) — change the
    // orientation depending on the output resolution.
    if t.config.default_orientation == Orientation::NO_ORIENTATION {
        let output_rect = t.outputs[output_idx].rect;
        for workspace in &workspaces {
            if con_num_children(t, *workspace) > 1 {
                continue;
            }

            t.cons[*workspace].layout = if output_rect.height > output_rect.width {
                Layout::L_SPLITV
            } else {
                Layout::L_SPLITH
            };

            if let Some(child) = t.cons[*workspace].nodes_head.front().copied() {
                let child_layout = t.cons[child].layout;
                if child_layout == Layout::L_SPLITV || child_layout == Layout::L_SPLITH {
                    let ws_layout = t.cons[*workspace].layout;
                    t.cons[child].layout = ws_layout;
                }
            }
        }
    }
}

/// C: src/randr.c:583 randr_query_outputs_15
///
/// Queries the outputs via RandR 1.5 (monitors). Returns false when the query
/// was not possible so that randr_query_outputs falls back to 1.4.
pub fn randr_query_outputs_15(t: &mut Tree, x: &mut dyn XConn, state: &mut RandrState) -> bool {
    // C: RandR 1.5 available at run-time (supported by the server and not
    // disabled by the user)
    if !state.has_randr_1_5 {
        return false;
    }
    debuglog("Querying outputs using RandR 1.5");
    let root = x.root_screen().root;
    let monitors = match x.randr_get_monitors(root) {
        Some(monitors) => monitors,
        None => {
            // C: ELOG("Could not get RandR monitors: X11 error code %d\n")
            errorlog("Could not get RandR monitors");
            // C: Fall back to RandR <= 1.4
            return false;
        }
    };

    // C: mark all outputs as to_be_disabled, since xcb_randr_get_monitors()
    // will only return active outputs.
    for idx in 0..t.outputs.len() {
        if Some(idx) != state.root_output {
            t.outputs[idx].to_be_disabled = true;
        }
    }

    debuglog(&format!("{} RandR monitors found", monitors.len()));

    for monitor in &monitors {
        // C: xcb_get_atom_name(monitor_info->name)
        let name = match x.atom_name(monitor.name) {
            Some(name) => name,
            None => {
                errorlog("Could not get RandR monitor name");
                continue;
            }
        };

        let update = if get_output_by_name(t, &name, false).is_none() {
            // C: register the associated output names in addition to the
            // monitor name, then insert the monitor name last so that it is
            // used as the primary name.
            let mut names_head: std::collections::VecDeque<output_name> =
                std::collections::VecDeque::new();
            for randr_output in &monitor.outputs {
                if let Some(info) = x.randr_get_output_info(*randr_output, 0) {
                    if info.crtc != XCB_NONE && info.name != name {
                        names_head.push_front(output_name { name: info.name });
                    }
                }
            }
            names_head.push_front(output_name { name: name.clone() });

            t.outputs.push_back(Output {
                id: 0,
                active: true,
                changed: false,
                to_be_disabled: false,
                primary: monitor.primary,
                names_head,
                con: None,
                rect: Rect::new(0, 0, 0, 0),
            });
            t.outputs.len() - 1
        } else {
            get_output_by_name(t, &name, false).expect("output vanished")
        };

        // C: we specified get_active == true, so we only receive active outputs
        t.outputs[update].active = true;
        t.outputs[update].to_be_disabled = false;
        t.outputs[update].primary = monitor.primary;

        let x_pos = monitor.x.max(0) as u32;
        let y_pos = monitor.y.max(0) as u32;
        let width = u32::from(monitor.width);
        let height = u32::from(monitor.height);

        let update_x = update_if_necessary(&mut t.outputs[update].rect.x, x_pos);
        let update_y = update_if_necessary(&mut t.outputs[update].rect.y, y_pos);
        let update_w = update_if_necessary(&mut t.outputs[update].rect.width, width);
        let update_h = update_if_necessary(&mut t.outputs[update].rect.height, height);

        t.outputs[update].changed = update_x || update_y || update_w || update_h;

        debuglog(&format!(
            "name {}, x {}, y {}, width {} px, height {} px, width {} mm, height {} mm, primary {}, automatic {}",
            name,
            monitor.x,
            monitor.y,
            monitor.width,
            monitor.height,
            monitor.width_in_millimeters,
            monitor.height_in_millimeters,
            monitor.primary,
            monitor.automatic
        ));
    }

    true
}

/// C: src/randr.c:748 handle_output
///
/// Called by randr_query_outputs_14 for each output: adds new outputs, updates
/// the rect of existing ones and marks disabled outputs as to_be_disabled.
fn handle_output(
    t: &mut Tree,
    x: &mut dyn XConn,
    id: u32,
    output: &OutputInfo,
    cts: u32,
    primary: Option<u32>,
) {
    let existing = get_output_by_id(t, id);
    let is_existing = existing.is_some();
    let idx = match existing {
        Some(idx) => idx,
        None => {
            t.outputs.push_back(Output {
                id,
                active: false,
                changed: false,
                to_be_disabled: false,
                primary: false,
                names_head: std::collections::VecDeque::new(),
                con: None,
                rect: Rect::new(0, 0, 0, 0),
            });
            t.outputs.len() - 1
        }
    };

    t.outputs[idx].id = id;
    t.outputs[idx].primary = primary == Some(id);
    // C: while (!SLIST_EMPTY(&new->names_head)) { free ... } then insert the
    // output name at the head.
    t.outputs[idx].names_head.clear();
    t.outputs[idx].names_head.push_front(output_name { name: output.name.clone() });

    debuglog(&format!("found output with name {}", output.name));

    // C: even if no CRTC is used at the moment, we store the output so that we
    // do not need to change the list ever again.
    if output.crtc == XCB_NONE {
        if is_existing && t.outputs[idx].active {
            t.outputs[idx].to_be_disabled = true;
        }
        return;
    }

    // C: xcb_randr_get_crtc_info(conn, output->crtc, cts)
    let crtc = match x.randr_get_crtc_info(output.crtc, cts) {
        Some(crtc) => crtc,
        None => {
            debuglog(&format!("Skipping output {}: could not get CRTC", output.name));
            return;
        }
    };

    let x_pos = crtc.x.max(0) as u32;
    let y_pos = crtc.y.max(0) as u32;
    let width = u32::from(crtc.width);
    let height = u32::from(crtc.height);

    let update_x = update_if_necessary(&mut t.outputs[idx].rect.x, x_pos);
    let update_y = update_if_necessary(&mut t.outputs[idx].rect.y, y_pos);
    let update_w = update_if_necessary(&mut t.outputs[idx].rect.width, width);
    let update_h = update_if_necessary(&mut t.outputs[idx].rect.height, height);
    let updated = update_x || update_y || update_w || update_h;

    t.outputs[idx].active = t.outputs[idx].rect.width != 0 && t.outputs[idx].rect.height != 0;
    if !t.outputs[idx].active {
        debuglog("width/height 0/0, disabling output");
        return;
    }

    debuglog(&format!(
        "mode: {}x{}+{}+{}",
        t.outputs[idx].rect.width,
        t.outputs[idx].rect.height,
        t.outputs[idx].rect.x,
        t.outputs[idx].rect.y
    ));

    // C: if we don't need to change an existing output or if the output does
    // not exist in the first place, the case is simple.
    if !updated || !is_existing {
        return;
    }

    t.outputs[idx].changed = true;
}

/// C: src/randr.c:800 randr_query_outputs_14
///
/// The RandR <= 1.4 path: iterate the outputs of the screen resources and ask
/// for their CRTC.
pub fn randr_query_outputs_14(t: &mut Tree, x: &mut dyn XConn, _state: &mut RandrState) {
    debuglog("Querying outputs using RandR <= 1.4");
    let root = x.root_screen().root;

    // C: xcb_randr_get_output_primary
    let primary = match x.randr_get_output_primary(root) {
        Some(primary) => Some(primary),
        None => {
            errorlog("Could not get RandR primary output");
            None
        }
    };

    // C: xcb_randr_get_screen_resources_current
    let (cts, randr_outputs) = match x.randr_get_screen_resources(root) {
        Some(resources) => resources,
        None => {
            errorlog("Could not query screen resources.");
            return;
        }
    };

    for id in randr_outputs {
        // C: xcb_randr_get_output_info(conn, randr_outputs[i], cts)
        let output = match x.randr_get_output_info(id, cts) {
            Some(output) => output,
            None => continue,
        };
        handle_output(t, x, id, &output, cts, primary);
    }
}

/// C: src/randr.c:875 move_content
///
/// Moves the content of an output container to the first (active) output.
fn move_content(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    let first = match t.outputs.iter().position(|output| output.active) {
        Some(first) => first,
        None => {
            // C: get_first_output() would be NULL here; bail out instead of
            // dereferencing it.
            errorlog("move_content: no active output to move the content to");
            return;
        }
    };
    let first_con = t.outputs[first].con.expect("move_content: first output has no con");
    let first_content = output_get_content(t, first_con).expect("move_content: no content");
    let first_rect = t.outputs[first].rect;
    let con_rect = t.cons[con].rect;

    // C: 1. Get the con to focus next
    let next = t.focused;

    // C: 2. iterate through the workspaces and re-assign them, fixing the
    // coordinates of floating containers as we go.
    let old_content = output_get_content(t, con).expect("move_content: no content");
    loop {
        let current = match t.cons[old_content].nodes_head.front().copied() {
            Some(current) => current,
            None => break,
        };
        if Some(current) != next && t.cons[current].focus_head.is_empty() {
            // C: the workspace is empty and not focused, get rid of it
            debuglog(&format!("Getting rid of {:?} (empty, unfocused)", current));
            tree_close_internal(t, b, current, KillWindow::DONT_KILL_WINDOW, false);
            continue;
        }

        con_detach(t, current);
        con_attach(t, b, current, first_content, false);

        let floats: Vec<ConId> = t.cons[current].floating_head.iter().copied().collect();
        for floating_con in floats {
            floating_fix_coordinates(t, floating_con, &con_rect, &first_rect);
        }
    }

    // C: restore the focus after con_detach / con_attach; next can be NULL.
    if let Some(next) = next {
        if t.cons.get(next).is_some() {
            con_focus(t, b, next);
            if let Some(ws) = con_get_workspace(t, next) {
                workspace_show(t, b, ws);
            }
        }
    }

    // C: 3. move the dock clients to the first output
    let dockareas: Vec<ConId> = t.cons[con]
        .nodes_head
        .iter()
        .copied()
        .filter(|child| t.cons[*child].r#type == ConType::CT_DOCKAREA)
        .collect();
    for dockarea in dockareas {
        loop {
            let dock = match t.cons[dockarea].nodes_head.front().copied() {
                Some(dock) => dock,
                None => break,
            };
            let window = t.cons[dock].window;
            let nc = match crate::con::con_for_window(t, first_con, window) {
                Some(nc) => nc,
                None => break,
            };
            if nc == dockarea {
                break;
            }
            con_detach(t, dock);
            con_attach(t, b, dock, nc, false);
        }
    }

    debuglog(&format!("Destroying disappearing con {:?}", con));
    tree_close_internal(t, b, con, KillWindow::DONT_KILL_WINDOW, true);
}

/// C: src/randr.c:926 randr_query_outputs
///
/// (Re-)queries the outputs via RandR and stores them in the list of outputs.
/// If no outputs are found, the output covering the root window is used.
pub fn randr_query_outputs(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, state: &mut RandrState) {
    if !randr_query_outputs_15(t, x, state) {
        randr_query_outputs_14(t, x, state);
    }

    // C: if there's no randr output, enable the output covering the root window.
    if any_randr_output_active(t) {
        debuglog("Active RandR output found. Disabling root output.");
        if let Some(root_output) = state.root_output {
            if root_output < t.outputs.len() && t.outputs[root_output].active {
                t.outputs[root_output].to_be_disabled = true;
            }
        }
    } else {
        debuglog("No active RandR output found. Enabling root output.");
        if let Some(root_output) = state.root_output {
            if root_output < t.outputs.len() {
                t.outputs[root_output].active = true;
            }
        }
    }

    // C: check for clones, disable the clones and reduce the mode to the
    // lowest common mode.
    let output_count = t.outputs.len();
    for idx in 0..output_count {
        if !t.outputs[idx].active || t.outputs[idx].to_be_disabled {
            continue;
        }
        debuglog(&format!(
            "output {} / {}, position ({}, {}), checking for clones",
            idx,
            output_primary_name(t, idx),
            t.outputs[idx].rect.x,
            t.outputs[idx].rect.y
        ));

        for other in (idx + 1)..output_count {
            if !t.outputs[other].active || t.outputs[other].to_be_disabled {
                continue;
            }

            if t.outputs[other].rect.x != t.outputs[idx].rect.x
                || t.outputs[other].rect.y != t.outputs[idx].rect.y
            {
                continue;
            }

            let width = t.outputs[other].rect.width.min(t.outputs[idx].rect.width);
            let height = t.outputs[other].rect.height.min(t.outputs[idx].rect.height);

            let update_w = update_if_necessary(&mut t.outputs[idx].rect.width, width);
            let update_h = update_if_necessary(&mut t.outputs[idx].rect.height, height);
            if update_w || update_h {
                t.outputs[idx].changed = true;
            }

            update_if_necessary(&mut t.outputs[other].rect.width, width);
            update_if_necessary(&mut t.outputs[other].rect.height, height);

            debuglog(&format!("disabling output {} ({})", other, output_primary_name(t, other)));
            t.outputs[other].to_be_disabled = true;
        }
    }

    // C: ensure that all outputs which are active also have a con. This is
    // necessary because in the next step, a clone might get disabled.
    for idx in 0..t.outputs.len() {
        if t.outputs[idx].active && t.outputs[idx].con.is_none() {
            debuglog(&format!(
                "Need to initialize a Con for output {}",
                output_primary_name(t, idx)
            ));
            output_init_con(t, b, idx);
            t.outputs[idx].changed = false;
        }
    }

    // C: ensure that all containers with type CT_OUTPUT have a valid
    // corresponding entry in outputs (#3767).
    let croot = t.croot.expect("randr_query_outputs: no croot");
    let children: Vec<ConId> = t.cons[croot].nodes_head.iter().copied().collect();
    for con in children {
        if con_is_internal(t, con) {
            continue;
        }
        let name = t.cons[con].name.clone().unwrap_or_default();
        if get_output_by_name(t, &name, true).is_none() {
            debuglog(&format!("No output {} found, moving its old content to first output", name));
            move_content(t, b, con);
        }
    }

    // C: handle outputs which have a new mode or are disabled now.
    let output_count = t.outputs.len();
    for idx in 0..output_count {
        if idx >= t.outputs.len() {
            break;
        }
        if t.outputs[idx].to_be_disabled {
            randr_disable_output(t, b, idx);
        }

        if idx < t.outputs.len() && t.outputs[idx].changed {
            output_change_mode(t, idx);
            t.outputs[idx].changed = false;
        }
    }

    // C: just go through each active output and assign one workspace.
    let output_count = t.outputs.len();
    for idx in 0..output_count {
        if idx >= t.outputs.len() || !t.outputs[idx].active {
            continue;
        }
        let con = match t.outputs[idx].con {
            Some(con) => con,
            None => continue,
        };
        let content = match output_get_content(t, con) {
            Some(content) => content,
            None => continue,
        };
        if t.cons[content].nodes_head.front().is_some() {
            continue;
        }
        debuglog(&format!("Should add ws for output {}", output_primary_name(t, idx)));
        init_ws_for_output(t, b, idx);
    }

    // C: focus the primary screen, if possible.
    let output_count = t.outputs.len();
    for idx in 0..output_count {
        if idx >= t.outputs.len() || !t.outputs[idx].primary || t.outputs[idx].con.is_none() {
            continue;
        }

        debuglog(&format!("Focusing primary output {}", output_primary_name(t, idx)));
        let con = t.outputs[idx].con.expect("focused primary output has no con");
        let content = match output_get_content(t, con) {
            Some(content) => content,
            None => continue,
        };
        if let Some(ws) = t.cons[content].focus_head.front().copied() {
            workspace_show(t, b, ws);
        }
    }

    // C: render_layout flushes
    b.ewmh_update_desktop_properties(t);
    tree_render(t, b);
}

/// C: src/randr.c:1062 randr_disable_output
///
/// Disables the output and moves its content.
pub fn randr_disable_output(t: &mut Tree, b: &mut dyn ConBackend, output_idx: OutputId) {
    debug_assert!(t.outputs[output_idx].to_be_disabled);

    t.outputs[output_idx].active = false;
    debuglog(&format!(
        "Output {} disabled, re-assigning workspaces/docks",
        output_primary_name(t, output_idx)
    ));

    if let Some(con) = t.outputs[output_idx].con.take() {
        // C: clear the pointer before move_content calls tree_close_internal
        move_content(t, b, con);
    }

    t.outputs[output_idx].to_be_disabled = false;
    t.outputs[output_idx].changed = false;
}

/// C: src/randr.c:1077 fallback_to_root_output
pub fn fallback_to_root_output(t: &mut Tree, b: &mut dyn ConBackend, state: &mut RandrState) {
    let root_output = match state.root_output {
        Some(root_output) => root_output,
        None => return,
    };
    t.outputs[root_output].active = true;
    output_init_con(t, b, root_output);
    init_ws_for_output(t, b, root_output);
}

/// C: src/randr.c:1090 randr_init
///
/// We have just established a connection to the X server and need the initial
/// XRandR information to setup workspaces for each screen.
pub fn randr_init(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    state: &mut RandrState,
    event_base: &mut i32,
    disable_randr15: bool,
) {
    // C: root_output = create_root_output(conn); TAILQ_INSERT_TAIL(&outputs, ...)
    let root_output = create_root_output(x);
    t.outputs.push_back(root_output);
    state.root_output = Some(t.outputs.len() - 1);

    // C: xcb_get_extension_data(conn, &xcb_randr_id)
    let extension = x.query_extension("RANDR");
    let present = extension.map(|(present, _)| present).unwrap_or(false);
    if !present {
        debuglog("RandR is not present, activating root output.");
        fallback_to_root_output(t, b, state);
        return;
    }

    // C: xcb_randr_query_version(conn, XCB_RANDR_MAJOR_VERSION,
    // XCB_RANDR_MINOR_VERSION)
    match x.randr_query_version() {
        Some((major, minor)) => {
            state.has_randr_1_5 = major >= 1 && minor >= 5 && !disable_randr15;
        }
        None => {
            errorlog("Could not query RandR version");
            fallback_to_root_output(t, b, state);
            return;
        }
    }

    randr_query_outputs(t, b, x, state);

    // C: if (event_base != NULL) *event_base = extension_reply->first_event;
    if let Some((_, first_event)) = extension {
        *event_base = i32::from(first_event);
        state.randr_base = *event_base;
    }

    // C: xcb_randr_select_input(conn, root, SCREEN_CHANGE | OUTPUT_CHANGE |
    // CRTC_CHANGE | OUTPUT_PROPERTY)
    let root = x.root_screen().root;
    x.randr_select_input(
        root,
        XCB_RANDR_NOTIFY_MASK_SCREEN_CHANGE
            | XCB_RANDR_NOTIFY_MASK_OUTPUT_CHANGE
            | XCB_RANDR_NOTIFY_MASK_CRTC_CHANGE
            | XCB_RANDR_NOTIFY_MASK_OUTPUT_PROPERTY,
    );

    // C: xcb_flush(conn)
    x.flush();
}

/****************************************************************************
 * xinerama.c
 ****************************************************************************/

/// C: src/xinerama.c:22 get_screen_at
fn get_screen_at(t: &Tree, x: u32, y: u32) -> Option<usize> {
    for (idx, output) in t.outputs.iter().enumerate() {
        if output.rect.x == x && output.rect.y == y {
            return Some(idx);
        }
    }
    None
}

/// C: src/xinerama.c:38 query_screens
///
/// Gets the Xinerama screens and converts them to virtual Outputs (only one
/// screen for two Xinerama screens which are configured in clone mode).
pub fn query_screens(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, screens: Vec<Rect>) {
    let mut num_screens = 0usize;
    for rect in screens {
        // C: Output *s = get_screen_at(x_org, y_org);
        let existing = get_screen_at(t, rect.x, rect.y);
        match existing {
            Some(s) => {
                debuglog(&format!("Re-used old Xinerama screen {s:?}\n"));
                // This screen already exists. We use the littlest screen so
                // that the user can always see the complete workspace.
                t.outputs[s].rect.width = t.outputs[s].rect.width.min(rect.width);
                t.outputs[s].rect.height = t.outputs[s].rect.height.min(rect.height);
            }
            None => {
                let mut names_head = std::collections::VecDeque::new();
                names_head.push_back(output_name { name: format!("xinerama-{num_screens}") });
                let new_output = Output {
                    id: 0,
                    active: true,
                    changed: false,
                    to_be_disabled: false,
                    primary: false,
                    names_head,
                    con: None,
                    rect,
                };
                debuglog(&format!(
                    "Created new Xinerama screen {}\n",
                    format_args!("xinerama-{num_screens}")
                ));
                // C: TAILQ_INSERT_HEAD for 0x0, TAILQ_INSERT_TAIL otherwise
                let idx = if rect.x == 0 && rect.y == 0 {
                    t.outputs.push_front(new_output);
                    0
                } else {
                    t.outputs.push_back(new_output);
                    t.outputs.len() - 1
                };
                // C: output_init_con(s); init_ws_for_output(s);
                output_init_con(t, b, idx);
                init_ws_for_output(t, b, idx);
                num_screens += 1;
            }
        }

        debuglog(&format!(
            "found Xinerama screen: {} x {} at {} x {}\n",
            rect.width, rect.height, rect.x, rect.y
        ));
    }

    // C: if (num_screens == 0) { ELOG(...); exit(EXIT_SUCCESS); }
    if num_screens == 0 && t.outputs.is_empty() {
        errorlog("No screens found. Please fix your setup. i3 will exit now.\n");
        std::process::exit(0);
    }

    // C: xcb_flush(conn) happens in randr_init/xinerama_init callers; the
    // screens must reach the server before the first tree_render.
    x.flush();
}

/// C: src/xinerama.c:96 use_root_output
///
/// This creates the root_output (borrowed from randr.c) and uses it as the
/// sole output for this session.
pub fn use_root_output(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn) {
    // C: Output *s = create_root_output(conn); s->active = true;
    //    TAILQ_INSERT_TAIL(&outputs, s, outputs);
    let mut s = create_root_output(x);
    s.active = true;
    t.outputs.push_back(s);
    let idx = t.outputs.len() - 1;
    // C: output_init_con(s); init_ws_for_output(s);
    output_init_con(t, b, idx);
    init_ws_for_output(t, b, idx);
}

/// C: src/xinerama.c:109 xinerama_init
///
/// We have just established a connection to the X server and need the initial
/// Xinerama information to setup workspaces for each screen.
pub fn xinerama_init(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    screens: Option<Vec<Rect>>,
) {
    match screens {
        Some(s) if !s.is_empty() => {
            query_screens(t, b, x, s);
        }
        _ => {
            use_root_output(t, b, x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{GeometryReply, NullBackend};
    use crate::xcb_util::RecordingConn;

    /// A RecordingConn that answers like a real RandR setup: one 1920x1080
    /// monitor named HDMI-1 backed by output 55 / CRTC 77.
    ///
    /// C: the replies of xcb_randr_get_monitors (src/randr.c:596) and of the
    /// 1.4 requests (src/randr.c:806-838).
    fn randr_15_conn() -> RecordingConn {
        use std::collections::{HashMap, VecDeque};
        let _ = VecDeque::<u8>::new();
        let mut x = RecordingConn::new(0x50);
        let name_atom = x.intern_named("HDMI-1");
        x.extensions.insert("RANDR", 130);
        x.randr_version = Some((1, 5));
        x.monitors = Some(vec![MonitorInfo {
            name: name_atom,
            primary: true,
            automatic: false,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            width_in_millimeters: 509,
            height_in_millimeters: 286,
            outputs: vec![55],
        }]);
        x.output_infos.insert(
            55,
            OutputInfo { name: "HDMI-1".to_string(), crtc: 77 },
        );
        x.crtc_infos.insert(
            77,
            GeometryReply { x: 0, y: 0, width: 1920, height: 1080, ..Default::default() },
        );
        x.screen_resources = Some((1000, vec![55]));
        x.output_primary = Some(55);
        let _ = HashMap::<u32, u8>::new();
        x
    }

    /// A RecordingConn for the RandR <= 1.4 path (monitors query fails).
    fn randr_14_conn() -> RecordingConn {
        let mut x = randr_15_conn();
        // C: xcb_randr_get_monitors failed -> randr_query_outputs_15 returns
        // false -> randr_query_outputs_14 runs.
        x.monitors = None;
        x.randr_version = Some((1, 4));
        x
    }

    fn initialized_tree() -> Tree {
        let mut t = Tree::new();
        crate::tree::tree_init(&mut t, &mut NullBackend, Rect::new(0, 0, 1920, 1080));
        t
    }

    /// The output con of the (single) active output's workspace list.
    fn active_output_con(t: &Tree) -> ConId {
        let idx = t.outputs.iter().position(|o| o.active).expect("an active output");
        t.outputs[idx].con.expect("output con")
    }

    #[test]
    fn randr_query_outputs_15_creates_output_con_and_workspace() {
        // C: src/randr.c:583 randr_query_outputs_15 + src/randr.c:456
        // init_ws_for_output: the output con lives directly below croot and
        // gets workspace "1" (created by create_workspace_on_output).
        let mut x = randr_15_conn();
        let mut t = initialized_tree();
        let mut b = NullBackend;
        let mut state = RandrState::default();
        let mut event_base = -1;

        randr_init(&mut t, &mut b, &mut x, &mut state, &mut event_base, false);

        assert!(state.has_randr_1_5, "RandR 1.5 is used");
        assert_eq!(event_base, 130, "the RandR event base is stored");
        assert!(x.log.iter().any(|l| l == "randr_get_monitors(0x50)"));
        assert!(
            x.log.iter().any(|l| l.starts_with("randr_select_input(0x50, mask=0xf)")),
            "the RandR events are selected: {}",
            x.log.join(" | ")
        );

        // One real output plus the inactive root output.
        assert_eq!(t.outputs.len(), 2);
        let output_idx = get_output_by_name(&t, "HDMI-1", true).expect("HDMI-1 is active");
        assert_eq!(t.outputs[output_idx].rect, Rect::new(0, 0, 1920, 1080));

        // (a) the output con is a direct child of croot
        let con = t.outputs[output_idx].con.expect("output con created");
        assert_eq!(t.cons[con].r#type, ConType::CT_OUTPUT);
        assert_eq!(t.cons[con].name.as_deref(), Some("HDMI-1"));
        assert_eq!(t.cons[con].parent, t.croot);
        assert_eq!(t.cons[con].layout, Layout::L_OUTPUT);
        // C: output_init_con also creates the top/bottom dockareas and content.
        let names: Vec<String> = t.cons[con]
            .nodes_head
            .iter()
            .map(|c| t.cons[*c].name.clone().unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["topdock", "content", "bottomdock"]);

        // (b) workspace "1" below the content container
        let content = output_get_content(&t, con).expect("content container");
        let workspaces: Vec<ConId> = t.cons[content].nodes_head.iter().copied().collect();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(t.cons[workspaces[0]].r#type, ConType::CT_WORKSPACE);
        assert_eq!(t.cons[workspaces[0]].name.as_deref(), Some("1"));
        assert_eq!(t.cons[workspaces[0]].num, 1);

        // (c) the workspace is focused
        assert_eq!(t.focused, Some(workspaces[0]));
        assert_eq!(con_get_workspace(&t, t.focused.unwrap()), Some(workspaces[0]));

        // the root output stays inactive
        let root = state.root_output.expect("root output");
        assert!(!t.outputs[root].active);
    }

    #[test]
    fn randr_query_outputs_14_fallback_used_when_15_fails() {
        // C: src/randr.c:926 — randr_query_outputs_15 returns false (X error)
        // and randr_query_outputs_14 builds the outputs from the screen
        // resources / CRTC replies.
        let mut x = randr_14_conn();
        let mut t = initialized_tree();
        let mut b = NullBackend;
        let mut state = RandrState::default();
        let mut event_base = -1;

        randr_init(&mut t, &mut b, &mut x, &mut state, &mut event_base, false);

        assert!(!state.has_randr_1_5, "RandR 1.4 was queried");
        assert!(
            x.log.iter().any(|l| l == "randr_get_screen_resources(0x50)"),
            "the 1.4 path ran: {}",
            x.log.join(" | ")
        );
        assert!(x.log.iter().any(|l| l.starts_with("randr_get_crtc_info(77")));

        let output_idx = get_output_by_id(&t, 55).expect("output 55 from the CRTC path");
        assert!(t.outputs[output_idx].active);
        assert!(t.outputs[output_idx].primary);
        assert_eq!(t.outputs[output_idx].names_head.front().unwrap().name, "HDMI-1");
        assert_eq!(t.outputs[output_idx].rect, Rect::new(0, 0, 1920, 1080));

        // the workspace is still created (C: the workspace loop at randr.c:1024)
        let con = t.outputs[output_idx].con.expect("output con");
        let content = output_get_content(&t, con).expect("content");
        let ws = *t.cons[content].nodes_head.front().expect("workspace");
        assert_eq!(t.cons[ws].name.as_deref(), Some("1"));
        assert_eq!(t.focused, Some(ws));
    }

    #[test]
    fn randr_switches_to_14_when_randr_15_is_disabled() {
        // C: randr_init stores has_randr_1_5 = version >= 1.5 && !disable_randr15
        let mut x = randr_15_conn();
        let mut t = initialized_tree();
        let mut b = NullBackend;
        let mut state = RandrState::default();
        let mut event_base = -1;

        randr_init(&mut t, &mut b, &mut x, &mut state, &mut event_base, true);

        assert!(!state.has_randr_1_5);
        assert!(!x.log.iter().any(|l| l.starts_with("randr_get_monitors")));
        assert!(x.log.iter().any(|l| l.starts_with("randr_get_screen_resources")));
        assert_eq!(t.outputs.iter().filter(|o| o.active).count(), 1);
    }

    #[test]
    fn randr_falls_back_to_root_output_without_randr() {
        // C: src/randr.c:1107 — RandR is not present, so fallback_to_root_output
        // creates the con + workspace for the root output.
        let mut x = RecordingConn::new(0x50);
        x.extensions.insert("RANDR", 0); // present == false
        let mut t = initialized_tree();
        let mut b = NullBackend;
        let mut state = RandrState::default();
        let mut event_base = -1;

        randr_init(&mut t, &mut b, &mut x, &mut state, &mut event_base, false);

        assert!(!state.has_randr_1_5);
        assert_eq!(event_base, -1);
        let con = active_output_con(&t);
        assert_eq!(t.cons[con].r#type, ConType::CT_OUTPUT);
        assert_eq!(t.cons[con].name.as_deref(), Some("xroot-0"));
        let content = output_get_content(&t, con).expect("content");
        let ws = *t.cons[content].nodes_head.front().expect("workspace");
        assert_eq!(t.cons[ws].name.as_deref(), Some("1"));
        assert_eq!(t.focused, Some(ws));
    }

    #[test]
    fn output_removal_moves_or_removes_the_workspaces() {
        // C: src/randr.c:926 randr_query_outputs: an output that disappears
        // from the RandR 1.5 monitor list is disabled, move_content() moves its
        // workspaces to the first output and drops the empty unfocused ones.
        let mut x = randr_15_conn();
        let mut t = initialized_tree();
        let mut b = NullBackend;
        let mut state = RandrState::default();
        let mut event_base = -1;
        randr_init(&mut t, &mut b, &mut x, &mut state, &mut event_base, false);

        let first_idx = get_output_by_name(&t, "HDMI-1", true).expect("HDMI-1");
        let first_con = t.outputs[first_idx].con.expect("con");
        let first_content = output_get_content(&t, first_con).expect("content");
        let ws1 = *t.cons[first_content].nodes_head.front().expect("ws 1");
        assert_eq!(t.cons[ws1].name.as_deref(), Some("1"));

        // A second monitor appears and gets workspace "2".
        let second_atom = x.intern_named("DP-1");
        x.monitors = Some(vec![
            MonitorInfo {
                name: first_atom(&x),
                primary: true,
                automatic: false,
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                width_in_millimeters: 509,
                height_in_millimeters: 286,
                outputs: vec![55],
            },
            MonitorInfo {
                name: second_atom,
                primary: false,
                automatic: false,
                x: 1920,
                y: 0,
                width: 1280,
                height: 1024,
                width_in_millimeters: 300,
                height_in_millimeters: 200,
                outputs: vec![56],
            },
        ]);
        x.output_infos
            .insert(56, OutputInfo { name: "DP-1".to_string(), crtc: 78 });
        x.crtc_infos.insert(
            78,
            GeometryReply { x: 1920, y: 0, width: 1280, height: 1024, ..Default::default() },
        );
        randr_query_outputs(&mut t, &mut b, &mut x, &mut state);

        let second_idx = get_output_by_name(&t, "DP-1", true).expect("DP-1 is active");
        let second_con = t.outputs[second_idx].con.expect("DP-1 con");
        let second_content = output_get_content(&t, second_con).expect("DP-1 content");
        assert_eq!(t.cons[second_content].nodes_head.len(), 1);

        // Focus workspace 2 so that it is not dropped as "empty and unfocused".
        let ws2 = *t.cons[second_content].nodes_head.front().expect("ws 2");
        workspace_show(&mut t, &mut b, ws2);
        assert_eq!(t.focused, Some(ws2));

        // DP-1 disappears: only HDMI-1 is left.
        x.monitors = Some(vec![MonitorInfo {
            name: first_atom(&x),
            primary: true,
            automatic: false,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            width_in_millimeters: 509,
            height_in_millimeters: 286,
            outputs: vec![55],
        }]);
        randr_query_outputs(&mut t, &mut b, &mut x, &mut state);

        // (e) the removed output keeps no con, its focused workspace was moved
        // to the first output and the empty one was closed.
        assert!(!t.outputs[second_idx].active);
        assert!(t.outputs[second_idx].con.is_none());
        assert!(get_output_by_name(&t, "DP-1", true).is_none());

        let moved: Vec<String> = t.cons[first_content]
            .nodes_head
            .iter()
            .map(|c| t.cons[*c].name.clone().unwrap_or_default())
            .collect();
        assert_eq!(moved, vec!["1".to_string(), "2".to_string()], "ws 2 moved to HDMI-1");
        assert!(t.cons.get(ws2).is_some(), "the focused workspace survived the move");
        assert_eq!(con_get_workspace(&t, t.focused.unwrap()), Some(ws2));
    }

    /// The atom of the first monitor (the HDMI-1 name atom of randr_15_conn).
    fn first_atom(x: &RecordingConn) -> u32 {
        x.atoms.get("HDMI-1").copied().expect("HDMI-1 atom")
    }

    #[test]
    fn randr_init_keeps_root_output_inactive_with_active_monitor() {
        // C: src/randr.c:930-940 — the root output is disabled as soon as a
        // real output is active.
        let mut x = randr_15_conn();
        let mut t = initialized_tree();
        let mut b = NullBackend;
        let mut state = RandrState::default();
        let mut event_base = -1;
        randr_init(&mut t, &mut b, &mut x, &mut state, &mut event_base, false);
        let root = state.root_output.expect("root output");
        assert!(!t.outputs[root].active);
        assert!(t.outputs[root].con.is_none());
    }

    #[test]
    fn get_output_by_id_finds_output() {
        // C: src/randr.c:34 get_output_by_id
        let mut t = Tree::new();
        t.outputs.push_back(Output {
            id: 99,
            active: true,
            changed: false,
            to_be_disabled: false,
            primary: false,
            names_head: [output_name { name: "LVDS-1".to_string() }].iter().cloned().collect(),
            con: None,
            rect: Rect::new(0, 0, 100, 100),
        });
        assert_eq!(get_output_by_id(&t, 99), Some(0));
        assert_eq!(get_output_by_id(&t, 100), None);
    }

    #[test]
    fn xinerama_falls_back_to_root_output() {
        // C: src/xinerama.c:109 xinerama_init — no screens -> root output
        let mut t = Tree::new();
        crate::tree::tree_init(&mut t, &mut NullBackend, Rect::new(0, 0, 1280, 800));
        let mut x = RecordingConn::new(0x50);
        xinerama_init(&mut t, &mut NullBackend, &mut x, None);
        assert_eq!(t.outputs.len(), 1);
        // C: src/randr.c:313 create_root_output names the output "xroot-0".
        assert_eq!(t.outputs[0].names_head.front().unwrap().name, "xroot-0");
        assert!(t.outputs[0].active);
        assert!(t.outputs[0].rect.width > 0);
    }

    #[test]
    fn get_screen_at_matches_rect_origin() {
        // C: src/xinerama.c:22 get_screen_at
        let mut t = Tree::new();
        t.outputs.push_back(Output {
            id: 1,
            active: true,
            changed: false,
            to_be_disabled: false,
            primary: false,
            names_head: [output_name { name: "A".to_string() }].iter().cloned().collect(),
            con: None,
            rect: Rect::new(1920, 0, 100, 100),
        });
        assert_eq!(get_screen_at(&t, 1920, 0), Some(0));
        assert_eq!(get_screen_at(&t, 0, 0), None);
    }

    #[test]
    fn randr_screen_change_event_requeries_the_outputs() {
        // C: src/handlers.c:454 handle_screen_change -> randr_query_outputs():
        // the RandR SCREEN_CHANGE_NOTIFY event re-queries the outputs and
        // creates the workspace for an output that appeared later.
        use crate::handlers::{handle_x11_event_ctx, EventContext};
        use x11rb::protocol::Event;

        let mut x = RecordingConn::new(0x50);
        let atoms = crate::xcb_util::Atoms::intern_all(&mut x);
        x.log.clear();
        x.geoms.insert(
            0x50,
            GeometryReply { x: 0, y: 0, width: 1920, height: 1080, ..Default::default() },
        );
        x.extensions.insert("RANDR", 100);
        x.randr_version = Some((1, 5));
        let name = x.intern_named("HDMI-1");
        x.monitors = Some(vec![MonitorInfo {
            name,
            primary: true,
            automatic: false,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            width_in_millimeters: 509,
            height_in_millimeters: 286,
            outputs: vec![55],
        }]);
        x.output_infos.insert(55, OutputInfo { name: "HDMI-1".to_string(), crtc: 77 });
        x.crtc_infos.insert(
            77,
            GeometryReply { x: 0, y: 0, width: 1920, height: 1080, ..Default::default() },
        );

        let mut t = initialized_tree();
        let mut b = NullBackend;
        let mut randr = RandrState::default();
        randr.has_randr_1_5 = true;
        let mut ctx = EventContext {
            root: 0x50,
            randr_base: 100,
            randr: Some(&mut randr),
            ..Default::default()
        };

        // raw extension event: response_type = randr_base + SCREEN_CHANGE_NOTIFY
        let mut raw = vec![0u8; 32];
        raw[0] = 100;
        let ev = Event::Unknown(raw);
        assert!(handle_x11_event_ctx(&mut t, &mut b, &mut x, &atoms, &ev, &mut ctx));

        assert!(
            x.log.iter().any(|l| l == "randr_get_monitors(0x50)"),
            "the outputs were re-queried: {}",
            x.log.join(" | ")
        );
        let idx = get_output_by_name(&t, "HDMI-1", true).expect("HDMI-1 is active");
        let con = t.outputs[idx].con.expect("output con");
        let content = output_get_content(&t, con).expect("content");
        let ws = *t.cons[content].nodes_head.front().expect("workspace");
        assert_eq!(t.cons[ws].name.as_deref(), Some("1"));
        assert_eq!(t.focused, Some(ws));
        let croot = t.croot.unwrap();
        assert_eq!(t.cons[croot].rect.width, 1920);
        assert_eq!(t.cons[croot].rect.height, 1080);
    }
}
