//! 1:1 port of src/scratchpad.c — moving windows to the scratchpad and
//! making them visible again.
//!
//! C: src/scratchpad.c

use crate::backend::ConBackend;
use crate::con::{
    con_activate, con_descend_focused, con_descend_tiling_focused, con_disable_fullscreen,
    con_get_output, con_get_workspace, con_inside_floating, con_is_internal, con_toggle_fullscreen,
    ConId,
};
use crate::data::{ConType, FullscreenMode, Rect, ScratchpadState};
use crate::floating::{floating_center, floating_enable, floating_fix_coordinates};
use crate::tree::Tree;
use crate::util::rect_equals;
use crate::workspace::{workspace_get, workspace_show};

/// C: src/scratchpad.c:18 `scratchpad_move`
///
/// Moves the specified window to the __i3_scratch workspace, making it
/// floating and setting the appropriate scratchpad_state.
///
/// Gets called upon the command 'move scratchpad'.
pub fn scratchpad_move(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        /* 'move scratchpad' used on a workspace. Calling it recursively on all
         * windows on this workspace. */
        let children: Vec<ConId> = t.cons[con].focus_head.iter().copied().collect();
        for current in children {
            scratchpad_move(t, b, current);
        }
        return;
    }

    let __i3_scratch = workspace_get(t, b, "__i3_scratch");
    if con_get_workspace(t, con) == Some(__i3_scratch) {
        /* This window is already on __i3_scratch. */
        return;
    }

    /* If the current con is in fullscreen mode, we need to disable that,
     * as a scratchpad window should never be in fullscreen mode */
    con_disable_fullscreen(t, b, con);

    /* 1: Ensure the window or any parent is floating. From now on, we deal
     * with the CT_FLOATING_CON. We use automatic == false because the user
     * made the choice that this window should be a scratchpad (and floating).
     */
    let maybe_floating_con = con_inside_floating(t, con);
    let con = if maybe_floating_con.is_none() {
        floating_enable(t, b, con, false);
        t.cons[con].parent.expect("scratchpad_move: no parent after floating_enable")
    } else {
        maybe_floating_con.expect("checked above")
    };

    /* 2: Send the window to the __i3_scratch workspace, mainting its
     * coordinates and not warping the pointer. */
    crate::con::con_move_to_workspace(t, b, con, __i3_scratch, true, true, false);

    /* 3: If this is the first time this window is used as a scratchpad, we set
     * the scratchpad_state to SCRATCHPAD_FRESH. The window will then be
     * adjusted in size according to what the user specifies. */
    if t.cons[con].scratchpad_state == ScratchpadState::SCRATCHPAD_NONE {
        if Some(con) == maybe_floating_con {
            /* It was in floating mode before, set scratchpad state to changed. */
            t.cons[con].scratchpad_state = ScratchpadState::SCRATCHPAD_CHANGED;
        } else {
            /* It was in tiling mode before, set scratchpad state to fresh. */
            t.cons[con].scratchpad_state = ScratchpadState::SCRATCHPAD_FRESH;
        }
    }
}

/// C: src/scratchpad.c:86 `scratchpad_show`
///
/// Either shows the top-most scratchpad window (con == None) or shows the
/// specified con (if it is scratchpad window).
///
/// When called with con == None and the currently focused window is a
/// scratchpad window, this serves as a shortcut to hide it again (so the user
/// can press the same key to quickly look something up).
pub fn scratchpad_show(t: &mut Tree, b: &mut dyn ConBackend, con: Option<ConId>) -> bool {
    let __i3_scratch = workspace_get(t, b, "__i3_scratch");
    let con = con;
    let focused = t.focused.expect("scratchpad_show: no focused con");

    /* If this was 'scratchpad show' without criteria, we check if the
     * currently focused window is a scratchpad window and should be hidden
     * again. */
    if con.is_none() {
        if let Some(floating) = con_inside_floating(t, focused) {
            if t.cons[floating].scratchpad_state != ScratchpadState::SCRATCHPAD_NONE {
                /* Focused window is a scratchpad window, hiding it. */
                scratchpad_move(t, b, focused);
                return true;
            }
        }
    }

    /* If the current con or any of its parents are in fullscreen mode, we
     * first need to disable it before showing the scratchpad con. */
    let mut fs = Some(focused);
    while let Some(f) = fs {
        if t.cons[f].fullscreen_mode != FullscreenMode::CF_NONE {
            break;
        }
        fs = t.cons[f].parent;
    }

    if let Some(fs) = fs {
        if t.cons[fs].r#type != ConType::CT_WORKSPACE {
            con_toggle_fullscreen(t, b, fs, FullscreenMode::CF_OUTPUT as i32);
        }
    }

    /* If this was 'scratchpad show' without criteria, we check if there is a
     * unfocused scratchpad on the current workspace and focus it */
    let mut focused_ws = con_get_workspace(t, focused);
    if let Some(fws) = focused_ws {
        let floating: Vec<ConId> = t.cons[fws].floating_head.iter().copied().collect();
        for walk_con in floating {
            if con.is_none() {
                if let Some(floating) = con_inside_floating(t, walk_con) {
                    if t.cons[floating].scratchpad_state != ScratchpadState::SCRATCHPAD_NONE
                        && Some(floating) != con_inside_floating(t, focused)
                    {
                        /* Found an unfocused scratchpad window on this workspace */
                        /* use con_descend_tiling_focused to get the last focused
                         * window inside this scratch container in order to
                         * keep the focus the same within this container */
                        con_activate(t, b, con_descend_tiling_focused(t, walk_con));
                        return true;
                    }
                }
            }
        }
    }

    /* If this was 'scratchpad show' without criteria, we check if there is a
     * visible scratchpad window on another workspace. In this case we move it
     * to the current workspace. */
    focused_ws = con_get_workspace(t, focused);
    let all_cons: Vec<ConId> = t.all_cons.iter().copied().collect();
    for walk_con in all_cons {
        let walk_ws = con_get_workspace(t, walk_con);
        if con.is_none() {
            if let Some(walk_ws) = walk_ws {
                if !con_is_internal(t, walk_ws) && focused_ws != Some(walk_ws) {
                    if let Some(floating) = con_inside_floating(t, walk_con) {
                        if t.cons[floating].scratchpad_state != ScratchpadState::SCRATCHPAD_NONE {
                            /* Found a visible scratchpad window on another
                             * workspace, moving it to this workspace. */
                            let fws = focused_ws.expect("scratchpad_show: no focused ws");
                            crate::con::con_move_to_workspace(t, b, floating, fws, true, false, false);
                            con_activate(t, b, con_descend_focused(t, walk_con));
                            return true;
                        }
                    }
                }
            }
        }
    }

    /* If this was 'scratchpad show' with criteria, we check if the window
     * is actually in the scratchpad */
    if let Some(con) = con {
        let parent = t.cons[con].parent;
        if parent.map(|p| t.cons[p].scratchpad_state == ScratchpadState::SCRATCHPAD_NONE).unwrap_or(true) {
            /* Window is not in the scratchpad, doing nothing. */
            return false;
        }
    }

    /* If this was 'scratchpad show' with criteria, we check if it matches a
     * currently visible scratchpad window and hide it. */
    let active = con_get_workspace(t, focused);
    let current = con.and_then(|c| con_get_workspace(t, c));
    if let Some(con) = con {
        if let Some(floating) = con_inside_floating(t, con) {
            if t.cons[floating].scratchpad_state != ScratchpadState::SCRATCHPAD_NONE
                && current != Some(__i3_scratch)
            {
                /* If scratchpad window is on the active workspace, then we
                 * should hide it, otherwise we should move it to the active
                 * workspace. */
                if current == active {
                    /* Window is a scratchpad window, hiding it. */
                    scratchpad_move(t, b, con);
                    return true;
                }
            }
        }
    }

    let con = match con {
        None => {
            /* Use the container on __i3_scratch which is highest in the focus
             * stack. When moving windows to __i3_scratch, they get inserted at
             * the bottom of the stack. */
            match t.cons[__i3_scratch].floating_head.front().copied() {
                None => {
                    /* You don't have any scratchpad windows yet.
                     * Use 'move scratchpad' to move a window to the scratchpad. */
                    return false;
                }
                Some(c) => c,
            }
        }
        Some(c) => {
            /* We used a criterion, so we need to do what follows (moving,
             * resizing) on the floating parent. */
            match con_inside_floating(t, c) {
                None => return false,
                Some(f) => f,
            }
        }
    };

    let active = match active {
        None => return false,
        Some(a) => a,
    };

    /* 1: Move the window from __i3_scratch to the current workspace. */
    crate::con::con_move_to_workspace(t, b, con, active, true, false, false);

    /* 2: Adjust the size if this window was not adjusted yet. */
    if t.cons[con].scratchpad_state == ScratchpadState::SCRATCHPAD_FRESH {
        /* Adjusting size of this window. */
        let output = con_get_output(t, con).expect("scratchpad_show: no output");
        t.cons[con].rect.width = (t.cons[output].rect.width as f64 * 0.5) as u32;
        t.cons[con].rect.height = (t.cons[output].rect.height as f64 * 0.75) as u32;
        /* TODO: floating_check_size(con, false); (size hints not modelled) */
        let ws_rect = con_get_workspace(t, con).map(|ws| t.cons[ws].rect).unwrap_or(Rect::default());
        floating_center(t, con, ws_rect);
    }

    /* Activate active workspace if window is from another workspace to ensure
     * proper focus. */
    if current != Some(active) {
        workspace_show(t, b, active);
    }

    con_activate(t, b, con_descend_focused(t, con));

    true
}

/// C: src/scratchpad.c:233 `_gcd` (static)
fn _gcd(m: i32, n: i32) -> i32 {
    if n == 0 {
        return m;
    }
    _gcd(n, m % n)
}

/// C: src/scratchpad.c:245 `_lcm` (static)
fn _lcm(m: i32, n: i32) -> i32 {
    let o = _gcd(m, n);
    (m * n) / o
}

/// C: src/scratchpad.c:262 `scratchpad_fix_resolution`
///
/// When starting i3 initially (and after each change to the connected
/// outputs), this function fixes the resolution of the __i3 pseudo-output.
pub fn scratchpad_fix_resolution(t: &mut Tree, b: &mut dyn ConBackend) {
    let __i3_scratch = workspace_get(t, b, "__i3_scratch");
    let __i3_output = con_get_output(t, __i3_scratch).expect("scratchpad_fix_resolution: no output");
    let croot = t.croot.expect("scratchpad_fix_resolution: no croot");
    let mut new_width = -1i32;
    let mut new_height = -1i32;
    let outputs: Vec<ConId> = t.cons[croot].nodes_head.iter().copied().collect();
    for output in outputs {
        if output == __i3_output {
            continue;
        }
        if new_width == -1 {
            new_width = t.cons[output].rect.width as i32;
            new_height = t.cons[output].rect.height as i32;
        } else {
            new_width = _lcm(new_width, t.cons[output].rect.width as i32);
            new_height = _lcm(new_height, t.cons[output].rect.height as i32);
        }
    }

    let old_rect = t.cons[__i3_output].rect;

    t.cons[__i3_output].rect.width = new_width as u32;
    t.cons[__i3_output].rect.height = new_height as u32;

    let new_rect = t.cons[__i3_output].rect;

    if rect_equals(new_rect, old_rect) {
        /* Scratchpad size unchanged. */
        return;
    }

    /* Fixing coordinates of scratchpad windows */
    let floating: Vec<ConId> = t.cons[__i3_scratch].floating_head.iter().copied().collect();
    for con in floating {
        floating_fix_coordinates(t, con, &old_rect, &new_rect);
    }
}
