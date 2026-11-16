//! 1:1 port of src/workspace.c — modifying workspaces, accessing them,
//! moving containers to workspaces.
//!
//! C: src/workspace.c

use crate::backend::ConBackend;
use crate::con::{
    con_attach, con_detach, con_descend_focused, con_focus, con_get_output, con_get_workspace,
    con_is_internal, con_new, con_num_children, get_focus_order, set_focus_order, ConId,
};
use crate::data::{
    Binding, ConType, FullscreenMode, KillWindow, Layout, Orientation, Rect, TimerHandle,
};
use crate::gaps::gaps_for_workspace;
use crate::output::{
    get_output_by_name, output_get_content, output_push_sticky_windows, OutputId,
};
use crate::tree::{tree_close_internal, Tree};
use crate::util::{name_is_digits, ws_name_to_number};
use crate::floating::floating_fix_coordinates;
// C: src/workspace.c:250 char *target_name = parse_string(&target, false);
use i3_spec::commands_parser::parse_string;

/// C: src/workspace.c:27 `get_existing_workspace_by_name`
///
/// Returns the workspace with the given name or None if such a workspace
/// does not exist.
pub fn get_existing_workspace_by_name(t: &Tree, name: &str) -> Option<ConId> {
    let croot = t.croot?;
    for &output in &t.cons[croot].nodes_head {
        let content = output_get_content(t, output)?;
        for &child in &t.cons[content].nodes_head {
            if t.cons[child].name.as_deref().map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false) {
                return Some(child);
            }
        }
    }
    None
}

/// C: src/workspace.c:43 `get_existing_workspace_by_num`
///
/// Returns the workspace with the given number or None if such a workspace
/// does not exist.
pub fn get_existing_workspace_by_num(t: &Tree, num: i32) -> Option<ConId> {
    let croot = t.croot?;
    for &output in &t.cons[croot].nodes_head {
        let content = output_get_content(t, output)?;
        for &child in &t.cons[content].nodes_head {
            if t.cons[child].num == num {
                return Some(child);
            }
        }
    }
    None
}

/// C: src/workspace.c:62 `_workspace_apply_default_orientation` (static)
///
/// Sets ws->layout to splith/splitv if default_orientation was specified in
/// the configfile. Otherwise, it uses splith/splitv depending on whether the
/// output is higher than wide.
fn _workspace_apply_default_orientation(t: &mut Tree, ws: ConId) {
    /* If default_orientation is set to NO_ORIENTATION we determine
     * orientation depending on output resolution. */
    if t.config.default_orientation == Orientation::NO_ORIENTATION {
        let output = con_get_output(t, ws).expect("_workspace_apply_default_orientation: no output");
        let rect = t.cons[output].rect;
        t.cons[ws].layout = if rect.height > rect.width { Layout::L_SPLITV } else { Layout::L_SPLITH };
        t.cons[ws].rect = rect;
    } else {
        t.cons[ws].layout = if t.config.default_orientation == Orientation::HORIZ { Layout::L_SPLITH } else { Layout::L_SPLITV };
    }
}

/// C: src/workspace.c:83 `get_assigned_output`
///
/// Returns the first output that is assigned to a workspace specified by the
/// given name or number. Returns None if no such output exists.
///
/// If an assignment matches by number but there is an assignment later that
/// matches by name, the second one is preferred.
/// The order of the 'ws_assignments' queue is respected: if multiple
/// assignments match the criteria, the first one is returned.
/// 'name' is ignored when None, 'parsed_num' is ignored when it is -1.
pub fn get_assigned_output(t: &Tree, name: Option<&str>, parsed_num: i64) -> Option<ConId> {
    let mut output: Option<ConId> = None;
    for assignment in &t.ws_assignments {
        let assignment_output = match &assignment.output {
            None => continue,
            Some(o) => o,
        };

        let name_matches = name.map(|n| assignment.name == n).unwrap_or(false);
        if name_matches {
            if let Some(assigned_by_name) = get_output_by_name(t, assignment_output, true) {
                /* When the name matches exactly, skip numbered assignments. */
                return t.outputs[assigned_by_name].con;
            }
        } else if output.is_none() /* Only keep the first numbered assignment. */
            && parsed_num != -1
            && name_is_digits(&assignment.name)
            && ws_name_to_number(&assignment.name) as i64 == parsed_num
        {
            if let Some(assigned_by_num) = get_output_by_name(t, assignment_output, true) {
                output = t.outputs[assigned_by_num].con;
            }
        }
    }

    output
}

/// C: src/workspace.c:134 `output_triggers_assignment`
///
/// Returns true if the first output assigned to a workspace with the given
/// workspace assignment is the same as the given output.
pub fn output_triggers_assignment(t: &Tree, output: OutputId, assignment: &crate::data::Workspace_Assignment) -> bool {
    let assigned = get_assigned_output(t, Some(assignment.name.as_str()), -1);
    assigned.is_some() && assigned == t.outputs[output].con
}

/// C: src/workspace.c:144 `workspace_get`
///
/// Returns a pointer to the workspace with the given number (starting at 0),
/// creating the workspace if necessary (by allocating the necessary amount of
/// memory and initializing the data structures correctly).
pub fn workspace_get(t: &mut Tree, b: &mut dyn ConBackend, num: &str) -> ConId {
    if let Some(workspace) = get_existing_workspace_by_name(t, num) {
        return workspace;
    }

    /* We set workspace->num to the number if this workspace's name begins with
     * a positive number. Otherwise it's a named ws and num will be 1. */
    let parsed_num = ws_name_to_number(num);

    let output = match get_assigned_output(t, Some(num), parsed_num as i64) {
        Some(o) => o,
        None => {
            /* if an assignment is not found, we create this workspace on the current output */
            let focused = t.focused.expect("workspace_get: no focused con");
            con_get_output(t, focused).expect("workspace_get: no output for focused con")
        }
    };

    /* No parent because we need to attach this container after setting its
     * type. con_attach will handle CT_WORKSPACEs differently. */
    let workspace = con_new(t, b, None, None);

    let name = format!("[i3 con] workspace {}", num);
    b.x_set_name(t, workspace, &name);

    t.cons[workspace].name = Some(num.to_string());
    t.cons[workspace].workspace_layout = t.config.default_layout;
    t.cons[workspace].num = parsed_num;
    t.cons[workspace].r#type = ConType::CT_WORKSPACE;
    t.cons[workspace].gaps = gaps_for_workspace(t, workspace);

    let content = output_get_content(t, output).expect("workspace_get: no content");
    con_attach(t, b, workspace, content, false);
    _workspace_apply_default_orientation(t, workspace);

    b.ipc_send_workspace_event(t, "init", workspace, None);
    b.ewmh_update_desktop_properties(t);

    workspace
}

/// C: src/workspace.c:206 `extract_workspace_names_from_bindings`
///
/// Extracts workspace names from keybindings (e.g. "web" from "bindsym
/// $mod+1 workspace web"), so that when an output needs a workspace, i3 can
/// start with the first configured one.
///
/// TODO: C uses the full commands_parser `parse_string` for the target name;
/// this port implements the common quoted/unquoted form only.
pub fn extract_workspace_names_from_bindings(t: &mut Tree) {
    t.binding_workspace_names.clear();

    let commands: Vec<String> = t.bindings.iter().map(|bind: &Binding| bind.command.clone()).collect();
    for command in commands {
        if command.len() < "workspace ".len() || !command[..9].eq_ignore_ascii_case("workspace") {
            continue;
        }
        let mut target = &command["workspace ".len()..];
        while target.starts_with(' ') || target.starts_with('\t') {
            target = &target[1..];
        }
        /* We check if this is the workspace
         * next/prev/next_on_output/prev_on_output/back_and_forth command.
         * Beware: The workspace names "next", "prev", "next_on_output",
         * "prev_on_output", "back_and_forth" and "current" are OK,
         * so we check before stripping the double quotes */
        if target.len() >= 4 && target[..4].eq_ignore_ascii_case("next") {
            continue;
        }
        if target.len() >= 4 && target[..4].eq_ignore_ascii_case("prev") {
            continue;
        }
        if target.len() >= 14 && target[..14].eq_ignore_ascii_case("next_on_output") {
            continue;
        }
        if target.len() >= 14 && target[..14].eq_ignore_ascii_case("prev_on_output") {
            continue;
        }
        if target.len() >= 15 && target[..15].eq_ignore_ascii_case("back_and_forth") {
            continue;
        }
        if target.len() >= 7 && target[..7].eq_ignore_ascii_case("current") {
            continue;
        }
        if target.len() >= 24 && target[..24].eq_ignore_ascii_case("--no-auto-back-and-forth") {
            target = &target[24..];
            while target.starts_with(' ') || target.starts_with('\t') {
                target = &target[1..];
            }
        }
        if target.len() >= 6 && target[..6].eq_ignore_ascii_case("number") {
            target = &target[6..];
            while target.starts_with(' ') || target.starts_with('\t') {
                target = &target[1..];
            }
        }
        /* C: char *target_name = parse_string(&target, false); — the
         * workspace name is a string token, so its delimiters are comma,
         * semicolon and newlines (src/commands_parser.c:95). Spaces stay part
         * of the name, which is what makes "workspace 1: eggs" work. */
        let mut walk = 0usize;
        let target_name = match parse_string(target.as_bytes(), &mut walk, false) {
            None => continue,
            Some(name) => name,
        };
        if target_name.is_empty() {
            continue;
        }
        if target_name.starts_with("__") {
            continue;
        }

        t.binding_workspace_names.push(target_name);
    }
}

/// C: src/workspace.c:268 `create_workspace_on_output`
///
/// Returns a pointer to a new workspace in the given output. The workspace
/// is created attached to the tree hierarchy through the given content
/// container.
pub fn create_workspace_on_output(t: &mut Tree, b: &mut dyn ConBackend, output: OutputId, content: ConId) -> ConId {
    /* add a workspace to this output */
    let mut exists = true;
    let ws = con_new(t, b, None, None);
    t.cons[ws].r#type = ConType::CT_WORKSPACE;

    /* try the configured workspace bindings first to find a free name */
    let binding_names: Vec<String> = t.binding_workspace_names.clone();
    for target_name in binding_names {
        /* Ensure that this workspace is not assigned to a different output —
         * otherwise we would create it, then move it over to its output, then
         * find a new workspace, etc… */
        let assigned = get_assigned_output(t, Some(&target_name), -1);
        if let Some(assigned) = assigned {
            if Some(assigned) != t.outputs[output].con {
                continue;
            }
        }

        let num = ws_name_to_number(&target_name);
        exists = if num == -1 {
            get_existing_workspace_by_name(t, &target_name).is_some()
        } else {
            get_existing_workspace_by_num(t, num).is_some()
        };
        if !exists {
            t.cons[ws].name = Some(target_name.clone());
            /* Set ->num to the number of the workspace, if the name actually
             * is a number or starts with a number */
            t.cons[ws].num = num;
            break;
        }
    }

    if exists {
        /* get the next unused workspace number */
        let mut c = 0;
        while exists {
            c += 1;
            let assigned = get_assigned_output(t, None, c as i64);
            exists = get_existing_workspace_by_num(t, c).is_some()
                || assigned.map(|a| Some(a) != t.outputs[output].con).unwrap_or(false);
        }
        t.cons[ws].num = c;
        t.cons[ws].name = Some(c.to_string());
    }

    con_attach(t, b, ws, content, false);

    let ws_name = t.cons[ws].name.clone().unwrap_or_default();
    let name = format!("[i3 con] workspace {}", ws_name);
    b.x_set_name(t, ws, &name);

    t.cons[ws].gaps = gaps_for_workspace(t, ws);

    t.cons[ws].fullscreen_mode = FullscreenMode::CF_OUTPUT;

    t.cons[ws].workspace_layout = t.config.default_layout;
    _workspace_apply_default_orientation(t, ws);

    b.ipc_send_workspace_event(t, "init", ws, None);
    ws
}

/// C: src/workspace.c:316 `workspace_is_visible`
///
/// Returns true if the workspace is currently visible. Especially important
/// for multi-monitor environments, as they can have multiple currently
/// active workspaces.
pub fn workspace_is_visible(t: &Tree, ws: ConId) -> bool {
    let output = match con_get_output(t, ws) {
        None => return false,
        Some(o) => o,
    };
    let fs = crate::con::con_get_fullscreen_con(t, output, FullscreenMode::CF_OUTPUT);
    fs == Some(ws)
}

/// C: src/workspace.c:327 `_get_sticky` (static)
fn _get_sticky(t: &Tree, con: ConId, sticky_group: &str, exclude: Option<ConId>) -> Option<ConId> {
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for current in nodes {
        if Some(current) != exclude
            && t.cons[current].sticky_group.is_some()
            && t.cons[current].window.is_some()
            && t.cons[current].sticky_group.as_deref() == Some(sticky_group)
        {
            return Some(current);
        }

        if let Some(recurse) = _get_sticky(t, current, sticky_group, exclude) {
            return Some(recurse);
        }
    }

    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for current in floating {
        if Some(current) != exclude
            && t.cons[current].sticky_group.is_some()
            && t.cons[current].window.is_some()
            && t.cons[current].sticky_group.as_deref() == Some(sticky_group)
        {
            return Some(current);
        }

        if let Some(recurse) = _get_sticky(t, current, sticky_group, exclude) {
            return Some(recurse);
        }
    }

    None
}

/// C: src/workspace.c:354 `workspace_reassign_sticky` (static)
///
/// Reassigns all child windows in sticky containers. Called when the user
/// changes workspaces.
fn workspace_reassign_sticky(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    /* 1: go through all containers */

    /* handle all children and floating windows of this node */
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for current in nodes {
        let sticky_group = match t.cons[current].sticky_group.clone() {
            None => {
                workspace_reassign_sticky(t, b, current);
                continue;
            }
            Some(sg) => sg,
        };

        /* 2: find a window which we can re-assign */
        let output = con_get_output(t, current).expect("workspace_reassign_sticky: no output");
        let src = match _get_sticky(t, output, &sticky_group, Some(current)) {
            None => {
                /* No window found for this sticky group */
                workspace_reassign_sticky(t, b, current);
                continue;
            }
            Some(s) => s,
        };

        b.x_move_win(t, src, current);
        let src_window = t.cons[src].window;
        t.cons[current].window = src_window;
        t.cons[current].mapped = true;
        t.cons[src].window = None;
        t.cons[src].mapped = false;

        b.x_reparent_child(t, current, src);
    }

    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for current in floating {
        workspace_reassign_sticky(t, b, current);
    }
}

/// C: src/workspace.c:395 `workspace_defer_update_urgent_hint_cb` (static)
///
/// Callback to reset the urgent flag of the given con to false. May be
/// started by workspace_show to avoid urgency hints being lost by switching
/// to a workspace focusing the con.
///
/// Exposed as a plain function so the real event loop can call it when the
/// TimerHandle fires (C wires it via ev_timer).
pub fn workspace_defer_update_urgent_hint_cb(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    t.cons[con].urgency_timer = None;

    if t.cons[con].urgent {
        crate::con::con_set_urgency(t, b, con, false);
        crate::con::con_update_parents_urgency(t, con);
        if let Some(ws) = con_get_workspace(t, con) {
            workspace_update_urgent_flag(t, b, ws);
        }
        b.ipc_send_window_event(t, "urgent", con);
        crate::tree::tree_render(t, b);
    }
}

/// C: src/workspace.c:412 `workspace_show`
///
/// Switches to the given workspace.
pub fn workspace_show(t: &mut Tree, b: &mut dyn ConBackend, workspace: ConId) {
    let mut old: Option<ConId> = None;

    /* safe-guard against showing i3-internal workspaces like __i3_scratch */
    if con_is_internal(t, workspace) {
        return;
    }

    /* disable fullscreen for the other workspaces and get the workspace we are
     * currently on. */
    let parent = t.cons[workspace].parent.expect("workspace_show: no parent");
    let siblings: Vec<ConId> = t.cons[parent].nodes_head.iter().copied().collect();
    for current in siblings {
        if t.cons[current].fullscreen_mode == FullscreenMode::CF_OUTPUT {
            old = Some(current);
        }
        t.cons[current].fullscreen_mode = FullscreenMode::CF_NONE;
    }

    /* enable fullscreen for the target workspace. If it happens to be the
     * same one we are currently on anyways, we can stop here. */
    t.cons[workspace].fullscreen_mode = FullscreenMode::CF_OUTPUT;
    let focused = t.focused.expect("workspace_show: no focused con");
    let current = con_get_workspace(t, focused);
    if Some(workspace) == current {
        /* Not switching, already there. */
        return;
    }

    /* Used to correctly update focus when pushing sticky windows. Holds the
     * previously focused container in the same output as workspace. For
     * example, if a sticky window is focused and then we switch focus to a
     * workspace in another output and then switch to a third workspace in the
     * first output, the sticky window needs to be refocused. */
    let mut old_focus: Option<ConId> = old.map(|o| con_descend_focused(t, o));

    /* Remember currently focused workspace for switching back to it later with
     * the 'workspace back_and_forth' command. */
    if let Some(current) = current {
        if !con_is_internal(t, current) {
            t.previous_workspace_name = t.cons[current].name.clone();
        }
    }

    workspace_reassign_sticky(t, b, workspace);

    let next = con_descend_focused(t, workspace);

    /* Memorize current output */
    let old_output = t.focused.and_then(|f| con_get_output(t, f));

    /* Display urgency hint for a while if the newly visible workspace would
     * focus and thereby immediately destroy it */
    if t.cons[next].urgent && (t.config.workspace_urgency_timer * 1000.0) as i32 > 0 {
        /* focus for now… */
        t.cons[next].urgent = false;
        con_focus(t, b, next);

        /* … but immediately reset urgency flags; they will be set to false by
         * the timer callback in case the container is focused at the time of
         * its expiration */
        let focused = t.focused.expect("workspace_show: no focused con");
        t.cons[focused].urgent = true;
        t.cons[workspace].urgent = true;

        if t.cons[focused].urgency_timer.is_none() {
            t.cons[focused].urgency_timer = Some(TimerHandle {
                data: focused,
                after: t.config.workspace_urgency_timer,
                repeat: t.config.workspace_urgency_timer,
            });
        } else {
            /* ev_timer_again: restart the timer (stub). */
            if let Some(timer) = t.cons[focused].urgency_timer.as_mut() {
                timer.after = t.config.workspace_urgency_timer;
            }
        }
    } else {
        con_focus(t, b, next);
    }

    b.ipc_send_workspace_event(t, "focus", workspace, current);

    /* Close old workspace if necessary. This must be done *after* doing
     * urgency handling, because tree_close_internal() will do a con_focus()
     * on the next client, which will clear the urgency flag too early. */
    if let Some(old) = old {
        if t.cons[old].nodes_head.is_empty() && t.cons[old].floating_head.is_empty() {
            /* check if this workspace is currently visible */
            if !workspace_is_visible(t, old) {
                b.ipc_send_workspace_event(t, "empty", old, None);
                tree_close_internal(t, b, old, KillWindow::DONT_KILL_WINDOW, false);

                /* Avoid calling output_push_sticky_windows later with a freed container. */
                if old_focus == Some(old) {
                    old_focus = None;
                }

                b.ewmh_update_desktop_properties(t);
            }
        }
    }

    t.cons[workspace].fullscreen_mode = FullscreenMode::CF_OUTPUT;

    /* Set mouse pointer */
    let new_output = t.focused.and_then(|f| con_get_output(t, f));
    if old_output != new_output {
        // C: src/workspace.c:559 x_set_warp_to(&next->rect);
        b.x_set_warp_to(t, Some(next));
    }

    /* Update the EWMH hints */
    b.ewmh_update_current_desktop(t);

    /* Push any sticky windows to the now visible workspace. */
    output_push_sticky_windows(t, b, old_focus);
}

/// C: src/workspace.c:536 `workspace_show_by_name`
///
/// Looks up the workspace by name and switches to it.
pub fn workspace_show_by_name(t: &mut Tree, b: &mut dyn ConBackend, name: &str) {
    let ws = workspace_get(t, b, name);
    workspace_show(t, b, ws);
}

/// C: src/workspace.c:543 `workspace_next`
///
/// Focuses the next workspace.
pub fn workspace_next(t: &Tree) -> Option<ConId> {
    let focused = t.focused?;
    let current = con_get_workspace(t, focused)?;
    let mut next: Option<ConId> = None;
    let mut first: Option<ConId> = None;
    let mut first_opposite: Option<ConId> = None;

    if t.cons[current].num == -1 {
        /* If currently a named workspace, find next named workspace. */
        if let Some(n) = t.nodes_next(current) {
            return Some(n);
        }
        let mut found_current = false;
        let croot = t.croot?;
        for &output in &t.cons[croot].nodes_head {
            /* Skip outputs starting with __, they are internal. */
            if con_is_internal(t, output) {
                continue;
            }
            let content = output_get_content(t, output)?;
            for &child in &t.cons[content].nodes_head {
                if t.cons[child].r#type != ConType::CT_WORKSPACE {
                    continue;
                }
                if first.is_none() {
                    first = Some(child);
                }
                let opposite_num = first_opposite.map(|f| t.cons[f].num).unwrap_or(i32::MIN);
                if first_opposite.is_none() || (t.cons[child].num != -1 && t.cons[child].num < opposite_num) {
                    first_opposite = Some(child);
                }
                if child == current {
                    found_current = true;
                } else if t.cons[child].num == -1 && found_current {
                    next = Some(child);
                    return next;
                }
            }
        }
    } else {
        /* If currently a numbered workspace, find next numbered workspace. */
        let mut found_current = false;
        let croot = t.croot?;
        for &output in &t.cons[croot].nodes_head {
            /* Skip outputs starting with __, they are internal. */
            if con_is_internal(t, output) {
                continue;
            }
            let content = output_get_content(t, output)?;
            for &child in &t.cons[content].nodes_head {
                if t.cons[child].r#type != ConType::CT_WORKSPACE {
                    continue;
                }
                let first_num = first.map(|f| t.cons[f].num).unwrap_or(i32::MAX);
                if first.is_none() || (t.cons[child].num != -1 && t.cons[child].num < first_num) {
                    first = Some(child);
                }
                if first_opposite.is_none() && t.cons[child].num == -1 {
                    first_opposite = Some(child);
                }
                if t.cons[child].num == -1 {
                    break;
                }
                /* Need to check child against current and next because we are
                 * traversing multiple lists and thus are not guaranteed the
                 * relative order between the list of workspaces. */
                let next_num = next.map(|n| t.cons[n].num).unwrap_or(i32::MAX);
                if t.cons[current].num < t.cons[child].num && t.cons[child].num < next_num {
                    next = Some(child);
                }

                /* If two workspaces have the same number, but different names
                 * (eg '5:a', '5:b') then just take the next one. */
                if child == current {
                    found_current = true;
                } else if found_current && t.cons[current].num == t.cons[child].num {
                    return Some(child);
                }
            }
        }
    }

    if next.is_none() {
        next = first_opposite.or(first);
    }

    next
}

/// C: src/workspace.c:609 `workspace_prev`
///
/// Focuses the previous workspace.
pub fn workspace_prev(t: &Tree) -> Option<ConId> {
    let focused = t.focused?;
    let current = con_get_workspace(t, focused)?;
    let mut prev: Option<ConId> = None;
    let mut first_opposite: Option<ConId> = None;
    let mut last: Option<ConId> = None;

    if t.cons[current].num == -1 {
        /* If named workspace, find previous named workspace. */
        let mut p = t.nodes_prev(current);
        if let Some(pp) = p {
            if t.cons[pp].num != -1 {
                p = None;
            }
        }
        prev = p;
        if prev.is_none() {
            let mut found_current = false;
            let croot = t.croot?;
            for &output in t.cons[croot].nodes_head.iter().rev() {
                /* Skip outputs starting with __, they are internal. */
                if con_is_internal(t, output) {
                    continue;
                }
                let content = output_get_content(t, output)?;
                for &child in t.cons[content].nodes_head.iter().rev() {
                    if t.cons[child].r#type != ConType::CT_WORKSPACE {
                        continue;
                    }
                    if last.is_none() {
                        last = Some(child);
                    }
                    let opposite_num = first_opposite.map(|f| t.cons[f].num).unwrap_or(i32::MIN);
                    if first_opposite.is_none() || (t.cons[child].num != -1 && t.cons[child].num > opposite_num) {
                        first_opposite = Some(child);
                    }
                    if child == current {
                        found_current = true;
                    } else if t.cons[child].num == -1 && found_current {
                        prev = Some(child);
                        return prev;
                    }
                }
            }
        }
    } else {
        /* If numbered workspace, find previous numbered workspace. */
        let mut found_current = false;
        let croot = t.croot?;
        for &output in t.cons[croot].nodes_head.iter().rev() {
            /* Skip outputs starting with __, they are internal. */
            if con_is_internal(t, output) {
                continue;
            }
            let content = output_get_content(t, output)?;
            for &child in t.cons[content].nodes_head.iter().rev() {
                if t.cons[child].r#type != ConType::CT_WORKSPACE {
                    continue;
                }
                let last_num = last.map(|l| t.cons[l].num).unwrap_or(i32::MIN);
                if last.is_none() || (t.cons[child].num != -1 && last_num < t.cons[child].num) {
                    last = Some(child);
                }
                if first_opposite.is_none() && t.cons[child].num == -1 {
                    first_opposite = Some(child);
                }
                if t.cons[child].num == -1 {
                    continue;
                }
                /* Need to check child against current and previous because we
                 * are traversing multiple lists and thus are not guaranteed
                 * the relative order between the list of workspaces. */
                let prev_num = prev.map(|p| t.cons[p].num).unwrap_or(i32::MIN);
                if t.cons[current].num > t.cons[child].num && t.cons[child].num > prev_num {
                    prev = Some(child);
                }

                /* If two workspaces have the same number, but different names
                 * (eg '5:a', '5:b') then just take the previous one. */
                if child == current {
                    found_current = true;
                } else if found_current && t.cons[current].num == t.cons[child].num {
                    return Some(child);
                }
            }
        }
    }

    if prev.is_none() {
        prev = first_opposite.or(last);
    }

    prev
}

/// C: src/workspace.c:690 `workspace_next_on_output`
///
/// Focuses the next workspace on the same output.
pub fn workspace_next_on_output(t: &Tree) -> Option<ConId> {
    let focused = t.focused?;
    let current = con_get_workspace(t, focused)?;
    let mut next: Option<ConId> = None;
    let output = con_get_output(t, focused)?;

    if t.cons[current].num == -1 {
        /* If currently a named workspace, find next named workspace. */
        next = t.nodes_next(current);
    } else {
        /* If currently a numbered workspace, find next numbered workspace. */
        let mut found_current = false;
        let content = output_get_content(t, output)?;
        for &child in &t.cons[content].nodes_head {
            if t.cons[child].r#type != ConType::CT_WORKSPACE {
                continue;
            }
            if t.cons[child].num == -1 {
                break;
            }
            /* Need to check child against current and next because we are
             * traversing multiple lists and thus are not guaranteed the
             * relative order between the list of workspaces. */
            let next_num = next.map(|n| t.cons[n].num).unwrap_or(i32::MAX);
            if t.cons[current].num < t.cons[child].num && t.cons[child].num < next_num {
                next = Some(child);
            }

            /* If two workspaces have the same number, but different names
             * (eg '5:a', '5:b') then just take the next one. */
            if child == current {
                found_current = true;
            } else if found_current && t.cons[current].num == t.cons[child].num {
                return Some(child);
            }
        }
    }

    /* Find next named workspace. */
    if next.is_none() {
        let mut found_current = false;
        let content = output_get_content(t, output)?;
        for &child in &t.cons[content].nodes_head {
            if t.cons[child].r#type != ConType::CT_WORKSPACE {
                continue;
            }
            if child == current {
                found_current = true;
            } else if t.cons[child].num == -1 && (t.cons[current].num != -1 || found_current) {
                next = Some(child);
                return next;
            }
        }
    }

    /* Find first workspace. */
    if next.is_none() {
        let content = output_get_content(t, output)?;
        for &child in &t.cons[content].nodes_head {
            if t.cons[child].r#type != ConType::CT_WORKSPACE {
                continue;
            }
            let next_num = next.map(|n| t.cons[n].num).unwrap_or(i32::MAX);
            if next.is_none() || (t.cons[child].num != -1 && t.cons[child].num < next_num) {
                next = Some(child);
            }
        }
    }

    next
}

/// C: src/workspace.c:757 `workspace_prev_on_output`
///
/// Focuses the previous workspace on same output.
pub fn workspace_prev_on_output(t: &Tree) -> Option<ConId> {
    let focused = t.focused?;
    let current = con_get_workspace(t, focused)?;
    let mut prev: Option<ConId> = None;
    let output = con_get_output(t, focused)?;

    if t.cons[current].num == -1 {
        /* If named workspace, find previous named workspace. */
        let mut p = t.nodes_prev(current);
        if let Some(pp) = p {
            if t.cons[pp].num != -1 {
                p = None;
            }
        }
        prev = p;
    } else {
        /* If numbered workspace, find previous numbered workspace. */
        let mut found_current = false;
        let content = output_get_content(t, output)?;
        for &child in t.cons[content].nodes_head.iter().rev() {
            if t.cons[child].r#type != ConType::CT_WORKSPACE || t.cons[child].num == -1 {
                continue;
            }
            /* Need to check child against current and previous because we
             * are traversing multiple lists and thus are not guaranteed
             * the relative order between the list of workspaces. */
            let prev_num = prev.map(|p| t.cons[p].num).unwrap_or(i32::MIN);
            if t.cons[current].num > t.cons[child].num && t.cons[child].num > prev_num {
                prev = Some(child);
            }

            /* If two workspaces have the same number, but different names
             * (eg '5:a', '5:b') then just take the previous one. */
            if child == current {
                found_current = true;
            } else if found_current && t.cons[current].num == t.cons[child].num {
                return Some(child);
            }
        }
    }

    /* Find previous named workspace. */
    if prev.is_none() {
        let mut found_current = false;
        let content = output_get_content(t, output)?;
        for &child in t.cons[content].nodes_head.iter().rev() {
            if t.cons[child].r#type != ConType::CT_WORKSPACE {
                continue;
            }
            if child == current {
                found_current = true;
            } else if t.cons[child].num == -1 && (t.cons[current].num != -1 || found_current) {
                prev = Some(child);
                return prev;
            }
        }
    }

    /* Find last workspace. */
    if prev.is_none() {
        let content = output_get_content(t, output)?;
        for &child in t.cons[content].nodes_head.iter().rev() {
            if t.cons[child].r#type != ConType::CT_WORKSPACE {
                continue;
            }
            let prev_num = prev.map(|p| t.cons[p].num).unwrap_or(i32::MIN);
            if prev.is_none() || t.cons[child].num > prev_num {
                prev = Some(child);
            }
        }
    }

    prev
}

/// C: src/workspace.c:819 `workspace_back_and_forth`
///
/// Focuses the previously focused workspace.
pub fn workspace_back_and_forth(t: &mut Tree, b: &mut dyn ConBackend) {
    let previous = match &t.previous_workspace_name {
        None => return,
        Some(p) => p.clone(),
    };

    workspace_show_by_name(t, b, &previous);
}

/// C: src/workspace.c:830 `workspace_back_and_forth_get`
///
/// Returns the previously focused workspace con, or None if unavailable.
pub fn workspace_back_and_forth_get(t: &mut Tree, b: &mut dyn ConBackend) -> Option<ConId> {
    let previous = t.previous_workspace_name.clone()?;

    Some(workspace_get(t, b, &previous))
}

/// C: src/workspace.c:843 `get_urgency_flag` (static)
fn get_urgency_flag(t: &Tree, con: ConId) -> bool {
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in nodes {
        if t.cons[child].urgent || get_urgency_flag(t, child) {
            return true;
        }
    }

    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for child in floating {
        if t.cons[child].urgent || get_urgency_flag(t, child) {
            return true;
        }
    }

    false
}

/// C: src/workspace.c:862 `workspace_update_urgent_flag`
///
/// Goes through all clients on the given workspace and updates the
/// workspace's urgent flag accordingly.
pub fn workspace_update_urgent_flag(t: &mut Tree, b: &mut dyn ConBackend, ws: ConId) {
    let old_flag = t.cons[ws].urgent;
    let new_flag = get_urgency_flag(t, ws);
    t.cons[ws].urgent = new_flag;

    if old_flag != new_flag {
        b.ipc_send_workspace_event(t, "urgent", ws, None);
    }
}

/// C: src/workspace.c:882 `ws_force_orientation`
///
/// 'Forces' workspace orientation by moving all cons into a new split-con
/// with the same layout as the workspace and then changing the workspace
/// layout.
pub fn ws_force_orientation(t: &mut Tree, b: &mut dyn ConBackend, ws: ConId, orientation: Orientation) {
    /* 1: create a new split container */
    let split = con_new(t, b, None, None);
    t.cons[split].parent = Some(ws);

    /* 2: copy layout from workspace */
    t.cons[split].layout = t.cons[ws].layout;

    /* 3: move the existing cons of this workspace below the new con */
    let focus_order = get_focus_order(t, ws);

    while let Some(child) = t.cons[ws].nodes_head.front().copied() {
        con_detach(t, child);
        con_attach(t, b, child, split, true);
    }

    set_focus_order(t, split, &focus_order);

    /* 4: switch workspace layout */
    t.cons[ws].layout = if orientation == Orientation::HORIZ { Layout::L_SPLITH } else { Layout::L_SPLITV };

    /* 5: attach the new split container to the workspace */
    con_attach(t, b, split, ws, false);

    /* 6: fix the percentages */
    crate::con::con_fix_percent(t, ws);
}

/// C: src/workspace.c:927 `workspace_attach_to`
///
/// Called when a new con (with a window, not an empty or split con) should
/// be attached to the workspace (for example when managing a new window or
/// when moving an existing window to the workspace level).
///
/// Depending on the workspace_layout setting, this function either returns
/// the workspace itself (default layout) or creates a new stacked/tabbed con
/// and returns that.
pub fn workspace_attach_to(t: &mut Tree, b: &mut dyn ConBackend, ws: ConId) -> ConId {
    if t.cons[ws].workspace_layout == Layout::L_DEFAULT {
        /* Default layout, just attaching it to the workspace itself. */
        return ws;
    }

    /* Non-default layout, creating a new split container */
    /* 1: create a new split container */
    let new = con_new(t, b, None, None);
    t.cons[new].parent = Some(ws);

    /* 2: set the requested layout on the split con */
    t.cons[new].layout = t.cons[ws].workspace_layout;

    /* 4: attach the new split container to the workspace */
    con_attach(t, b, new, ws, false);

    /* 5: fix the percentages */
    crate::con::con_fix_percent(t, ws);

    new
}

/// C: src/workspace.c:958 `workspace_encapsulate`
///
/// Creates a new container and re-parents all of children from the given
/// workspace into it.
///
/// The container inherits the layout from the workspace.
pub fn workspace_encapsulate(t: &mut Tree, b: &mut dyn ConBackend, ws: ConId) -> Option<ConId> {
    if t.cons[ws].nodes_head.is_empty() {
        /* Workspace has no children to encapsulate */
        return None;
    }

    let new = con_new(t, b, None, None);
    t.cons[new].parent = Some(ws);
    t.cons[new].layout = t.cons[ws].layout;

    let focus_order = get_focus_order(t, ws);

    while let Some(child) = t.cons[ws].nodes_head.front().copied() {
        con_detach(t, child);
        con_attach(t, b, child, new, true);
    }

    set_focus_order(t, new, &focus_order);

    con_attach(t, b, new, ws, true);

    Some(new)
}

/// C: src/workspace.c:995 `workspace_move_to_output`
///
/// Move the given workspace to the specified output.
pub fn workspace_move_to_output(t: &mut Tree, b: &mut dyn ConBackend, ws: ConId, output: OutputId) {
    let current_output = crate::output::get_output_for_con(t, ws).expect("workspace_move_to_output: no current output");
    let content = output_get_content(t, t.outputs[output].con.expect("workspace_move_to_output: output has no con"))
        .expect("workspace_move_to_output: no content");

    if t.cons[ws].parent == Some(content) {
        /* Nothing to do, workspace already there */
        return;
    }

    let previously_visible_ws = t.cons[content].focus_head.front().copied();

    let workspace_was_visible = workspace_is_visible(t, ws);
    if con_num_children(t, t.cons[ws].parent.expect("workspace_move_to_output: ws has no parent")) == 1 {
        /* Creating a new workspace to replace this one (last on its output). */

        /* check if we can find a workspace assigned to this output */
        let mut used_assignment = false;
        let assignments: Vec<crate::data::Workspace_Assignment> = t.ws_assignments.iter().cloned().collect();
        for assignment in assignments {
            if !output_triggers_assignment(t, current_output, &assignment) {
                continue;
            }
            /* check if this workspace's name or num is already attached to the tree */
            let num = ws_name_to_number(&assignment.name);
            let attached = if num == -1 {
                get_existing_workspace_by_name(t, &assignment.name).is_some()
            } else {
                get_existing_workspace_by_num(t, num).is_some()
            };
            if attached {
                continue;
            }

            /* so create the workspace referenced to by this assignment */
            workspace_get(t, b, &assignment.name);
            used_assignment = true;
            break;
        }

        /* if we couldn't create the workspace using an assignment, create it on
         * the output. Workspace init IPC events are sent either by
         * workspace_get or create_workspace_on_output. */
        if !used_assignment {
            let old_parent = t.cons[ws].parent.expect("workspace_move_to_output: ws has no parent");
            create_workspace_on_output(t, b, current_output, old_parent);
        }
    }

    /* detach from the old output and attach to the new output */
    let old_content = t.cons[ws].parent.expect("workspace_move_to_output: ws has no parent");
    con_detach(t, ws);
    if workspace_was_visible {
        /* The workspace which we just detached was visible, so focus the next
         * one in the focus-stack. */
        let focus_ws = t.cons[old_content].focus_head.front().copied().expect("workspace_move_to_output: no ws to focus");
        workspace_show(t, b, focus_ws);
    }
    con_attach(t, b, ws, content, false);

    /* fix the coordinates of the floating containers */
    let floating: Vec<ConId> = t.cons[ws].floating_head.iter().copied().collect();
    for floating_con in floating {
        let old_rect = t.cons[old_content].rect;
        let new_rect = t.cons[content].rect;
        floating_fix_coordinates(t, floating_con, &old_rect, &new_rect);
    }

    b.ipc_send_workspace_event(t, "move", ws, None);
    if workspace_was_visible {
        /* Focus the moved workspace on the destination output. */
        workspace_show(t, b, ws);
    }

    b.ewmh_update_desktop_properties(t);

    let previously_visible_ws = match previously_visible_ws {
        None => return,
        Some(p) => p,
    };

    /* NB: We cannot simply work with previously_visible_ws since it might have
     * been cleaned up by workspace_show() already, depending on the focus
     * order/number of other workspaces on the output. Instead, we loop through
     * the available workspaces and only work with previously_visible_ws if we
     * still find it. */
    let nodes: Vec<ConId> = t.cons[content].nodes_head.iter().copied().collect();
    for ws_ in nodes {
        if ws_ != previously_visible_ws {
            continue;
        }

        /* Call the on_remove_child callback of the workspace which previously
         * was visible on the destination output. Since it is no longer visible,
         * it might need to get cleaned up. */
        crate::con::con_on_remove_child(t, b, previously_visible_ws);
        break;
    }
}

/// The event-loop side of the deferred urgency reset timer.
///
/// C: src/workspace.c:504-516 `ev_timer_init(focused->urgency_timer,
/// workspace_defer_update_urgent_hint_cb, config.workspace_urgency_timer,
/// config.workspace_urgency_timer)` / `ev_timer_again()` — libev keeps the
/// timer in its own loop and calls back into workspace.c:414. The port has no
/// libev: the registration is stored on the con (Con::urgency_timer, the
/// `ev_timer *` of data.h), and this queue is the loop's view of it — it arms
/// a deadline when a registration appears, forgets it when the tree clears it
/// (ev_timer_stop + FREE) and reports the registrations that expired.
///
/// Like the C callback (which never re-arms), a timer fires once: the callback
/// clears Con::urgency_timer, and poll() drops a deadline whose handle is gone.
#[derive(Default)]
pub struct UrgencyTimerQueue {
    /// C: the next ev_timer expiry, which libev tracks internally.
    armed: std::collections::HashMap<ConId, std::time::Instant>,
}

impl UrgencyTimerQueue {
    /// C: the ev_timer watchers of the main loop — returns the cons whose timer
    /// expired, oldest deadline first.
    pub fn poll(&mut self, t: &Tree, now: std::time::Instant) -> Vec<ConId> {
        // ev_timer_start / ev_timer_again: a new registration arms the deadline
        // `after` seconds from now (an existing one keeps its deadline, which
        // is what ev_timer_again() only changes when the timer already expired).
        for &con in &t.all_cons {
            if let Some(timer) = &t.cons[con].urgency_timer {
                let after = if timer.after > 0.0 { timer.after } else { 0.0 };
                self.armed
                    .entry(con)
                    .or_insert_with(|| now + std::time::Duration::from_secs_f64(after));
            }
        }

        // ev_timer_stop + FREE(con->urgency_timer): the tree clears the handle.
        let cons = &t.cons;
        self.armed
            .retain(|con, _| cons.get(*con).map(|c| c.urgency_timer.is_some()).unwrap_or(false));

        let mut expired: Vec<(ConId, std::time::Instant)> = self
            .armed
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(con, deadline)| (*con, *deadline))
            .collect();
        expired.sort_by_key(|(_, deadline)| *deadline);
        for (con, _) in &expired {
            self.armed.remove(con);
        }
        expired.into_iter().map(|(con, _)| con).collect()
    }
}

// Rect is referenced by the C prototypes this module mirrors.
#[allow(unused)]
fn _rect_type_anchor(_r: Rect) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::tree::tree_init;
    use std::time::{Duration, Instant};

    /// C: workspace.c:504-516 arms the timer when a visible workspace would be
    /// focused with its urgency flag, and workspace.c:414's callback resets it.
    /// The queue must report the expiry exactly once and after `after` seconds.
    #[test]
    fn urgency_timer_fires_once_at_its_deadline() {
        let mut t = Tree::new();
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));

        let mut queue = UrgencyTimerQueue::default();
        let start = Instant::now();

        // No timer registered: nothing to report.
        assert!(queue.poll(&t, start).is_empty());

        let con = t.croot.expect("tree_init sets croot");
        t.cons[con].urgency_timer = Some(TimerHandle {
            data: con,
            after: 0.5,
            repeat: 0.5,
        });

        // Armed but not expired yet.
        assert!(queue.poll(&t, start).is_empty());
        assert!(queue.poll(&t, start + Duration::from_millis(499)).is_empty());

        // Expired: reported once (the C callback frees the timer).
        assert_eq!(queue.poll(&t, start + Duration::from_millis(500)), vec![con]);
        assert!(queue.poll(&t, start + Duration::from_secs(5)).is_empty());

        // The callback clears the handle; a cleared timer cannot fire again.
        t.cons[con].urgency_timer = None;
        assert!(queue.poll(&t, start + Duration::from_secs(10)).is_empty());
    }

    /// C: ev_timer_stop() removes the watcher — a deadline for a handle the
    /// tree dropped in the meantime must not fire later.
    #[test]
    fn cleared_timer_never_fires() {
        let mut t = Tree::new();
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));

        let mut queue = UrgencyTimerQueue::default();
        let start = Instant::now();
        let con = t.croot.expect("tree_init sets croot");
        t.cons[con].urgency_timer = Some(TimerHandle {
            data: con,
            after: 1.0,
            repeat: 1.0,
        });
        assert!(queue.poll(&t, start).is_empty());

        t.cons[con].urgency_timer = None;
        assert!(queue.poll(&t, start + Duration::from_secs(2)).is_empty());
    }
}
