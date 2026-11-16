//! Partial port of src/match.c — only match_matches_window(), which the
//! tree layer needs for con_for_window() and assignments.
//!
//! C: src/match.c

use crate::con::{con_by_window_id, con_get_workspace, con_inside_floating, ConId};
use crate::data::{
    FloatingState, Match, MatchDock, MatchWindowMode, WindowDock, WindowId,
};
use crate::tree::Tree;

/// C string view of a window property.
///
/// C: `i3Window` keeps WM_WINDOW_ROLE / WM_CLASS / WM_NAME in `char *` fields
/// filled with `sstrndup(xcb_get_property_value(...), length)`, and
/// `strcmp` / `regex_matches` stop at the first NUL byte. X11 STRING
/// properties carry a trailing NUL inside the property length, so C compares
/// `"special_role"` even when the property is `"special_role\\0"`. The port
/// stores the bytes verbatim in a Rust String, so the same truncation has to
/// happen here (otherwise anchored criteria like `^special_role$` never
/// match).
fn c_str(s: &str) -> &str {
    match s.find('\0') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// C: src/match.c:90 `match_matches_window`
///
/// Checks the match against the given window. `window` is a slot in
/// Tree::windows (the C `i3Window *`).
pub fn match_matches_window(t: &Tree, match_: &Match, window: Option<WindowId>) -> bool {
    let window = match window {
        Some(w) => w,
        None => return false,
    };

    macro_rules! check_window_field {
        ($match_field:ident, $window_field:ident) => {
            if let Some(m) = &match_.$match_field {
                let raw = t.windows[window].$window_field.clone().unwrap_or_default();
                let window_field_str = c_str(&raw);
                let focused_str: Option<&str> = t
                    .focused
                    .and_then(|f| t.cons[f].window)
                    .and_then(|w| t.windows[w].$window_field.as_deref())
                    .map(c_str);
                let matches_focused = m.pattern == "__focused__"
                    && t.focused.is_some()
                    && t.focused.and_then(|f| t.cons[f].window).is_some()
                    && focused_str.is_some()
                    && window_field_str == focused_str.unwrap();
                if matches_focused {
                    /* window matches focused window */
                } else if m.matches(window_field_str) {
                    /* window matches */
                } else {
                    return false;
                }
            }
        };
    }

    check_window_field!(class, class_class);
    check_window_field!(instance, class_instance);

    if match_.id != crate::data::XCB_WINDOW_NONE {
        if t.windows[window].id == match_.id {
            /* match made by window id */
        } else {
            return false;
        }
    }

    check_window_field!(title, name);
    check_window_field!(window_role, role);

    if match_.window_type != u32::MAX {
        if t.windows[window].window_type == match_.window_type {
            /* window_type matches */
        } else {
            return false;
        }
    }

    check_window_field!(machine, machine);

    let mut con: Option<ConId>;
    if match_.urgent == crate::data::MatchUrgency::U_LATEST {
        /* if the window isn't urgent, no sense in searching */
        if t.windows[window].urgent.tv_sec == 0 {
            return false;
        }
        /* if we find a window that is newer than this one, bail */
        for &c in &t.all_cons {
            if let Some(w) = t.cons[c].window {
                let a = &t.windows[w].urgent;
                let b = &t.windows[window].urgent;
                if (a.tv_sec, a.tv_usec) > (b.tv_sec, b.tv_usec) {
                    return false;
                }
            }
        }
    }

    if match_.urgent == crate::data::MatchUrgency::U_OLDEST {
        /* if the window isn't urgent, no sense in searching */
        if t.windows[window].urgent.tv_sec == 0 {
            return false;
        }
        /* if we find a window that is older than this one (and not 0), bail */
        for &c in &t.all_cons {
            if let Some(w) = t.cons[c].window {
                let a = &t.windows[w].urgent;
                if a.tv_sec != 0 {
                    let b = &t.windows[window].urgent;
                    if (a.tv_sec, a.tv_usec) < (b.tv_sec, b.tv_usec) {
                        return false;
                    }
                }
            }
        }
    }

    if let Some(m) = &match_.workspace {
        con = con_by_window_id(t, t.windows[window].id);
        if con.is_none() {
            return false;
        }
        let con = con.unwrap();

        let ws = match con_get_workspace(t, con) {
            None => return false,
            Some(ws) => ws,
        };

        let focused_ws_name = t
            .focused
            .and_then(|f| con_get_workspace(t, f))
            .and_then(|w| t.cons[w].name.clone());
        let ws_name = t.cons[ws].name.clone().unwrap_or_default();
        if m.pattern == "__focused__" && Some(ws_name.clone()) == focused_ws_name {
            /* workspace matches focused workspace */
        } else if m.matches(&ws_name) {
            /* workspace matches */
        } else {
            return false;
        }
    }

    if match_.dock != MatchDock::M_DONTCHECK {
        let dock = t.windows[window].dock;
        let ok = (dock == WindowDock::W_DOCK_TOP && match_.dock == MatchDock::M_DOCK_TOP)
            || (dock == WindowDock::W_DOCK_BOTTOM && match_.dock == MatchDock::M_DOCK_BOTTOM)
            || ((dock == WindowDock::W_DOCK_TOP || dock == WindowDock::W_DOCK_BOTTOM)
                && match_.dock == MatchDock::M_DOCK_ANY)
            || (dock == WindowDock::W_NODOCK && match_.dock == MatchDock::M_NODOCK);
        if ok {
            /* dock status matches */
        } else {
            return false;
        }
    }

    if let Some(m) = &match_.mark {
        con = con_by_window_id(t, t.windows[window].id);
        if con.is_none() {
            return false;
        }
        let con = con.unwrap();

        let mut matched = false;
        for mark in &t.cons[con].marks_head {
            if m.matches(&mark.name) {
                matched = true;
                break;
            }
        }

        if matched {
            /* mark matches */
        } else {
            return false;
        }
    }

    if match_.window_mode != MatchWindowMode::WM_ANY {
        con = con_by_window_id(t, t.windows[window].id);
        if con.is_none() {
            return false;
        }
        let con = con.unwrap();

        match match_.window_mode {
            MatchWindowMode::WM_TILING_AUTO => {
                if t.cons[con].floating != FloatingState::FLOATING_AUTO_OFF {
                    return false;
                }
            }
            MatchWindowMode::WM_TILING_USER => {
                if t.cons[con].floating != FloatingState::FLOATING_USER_OFF {
                    return false;
                }
            }
            MatchWindowMode::WM_TILING => {
                if con_inside_floating(t, con).is_some() {
                    return false;
                }
            }
            MatchWindowMode::WM_FLOATING_AUTO => {
                if t.cons[con].floating != FloatingState::FLOATING_AUTO_ON {
                    return false;
                }
            }
            MatchWindowMode::WM_FLOATING_USER => {
                if t.cons[con].floating != FloatingState::FLOATING_USER_ON {
                    return false;
                }
            }
            MatchWindowMode::WM_FLOATING => {
                if con_inside_floating(t, con).is_none() {
                    return false;
                }
            }
            MatchWindowMode::WM_ANY => {}
        }
    }

    if match_.con_id.is_some() {
        /* NB: the C code compares the con_id against the con holding the
         * window (see the full match.c port); the window lookup mirrors it. */
        let con_by_id = con_by_window_id(t, t.windows[window].id);
        if con_by_id != match_.con_id {
            return false;
        }
    }

    if match_.error.is_some() {
        return false;
    }

    true
}
