//! 1:1 port of src/assignments.c — assignments for specific windows
//! (for_window).
//!
//! C: src/assignments.c

use crate::backend::ConBackend;
use crate::data::{Assignment, AssignmentType, A_ANY, A_COMMAND, WindowId};
use crate::match_::match_matches_window;
use crate::tree::Tree;

/// C: src/assignments.c:15 `run_assignments`
///
/// Checks the list of assignments for the given window and runs all matching
/// ones (unless they have already been run for this specific window).
pub fn run_assignments(t: &mut Tree, b: &mut dyn ConBackend, window: Option<WindowId>) {
    let window = match window {
        None => return,
        Some(w) => w,
    };

    let old_first_assignment = t.assignments.front().map(|a| a as *const Assignment as usize);

    let mut needs_render = false;

    /* Check if any assignments match */
    let assignments: Vec<usize> = (0..t.assignments.len()).collect();
    for idx in assignments {
        let (type_, matches) = {
            let current = &t.assignments[idx];
            (current.type_, current.match_.clone())
        };
        if type_ != A_COMMAND || !match_matches_window(t, &matches, Some(window)) {
            continue;
        }

        let mut skip = false;
        {
            let win = &t.windows[window];
            for &ran in &win.ran_assignments {
                if ran != idx {
                    continue;
                }
                skip = true;
                break;
            }
        }

        if skip {
            continue;
        }

        /* Store that we ran this assignment to not execute it again. We have
         * to do this before running the actual command to prevent infinite
         * loops. */
        {
            let win = t.windows.get_mut(window).expect("run_assignments: window vanished");
            win.ran_assignments.push_back(idx);
            win.nr_assignments = win.ran_assignments.len() as u32;
        }

        let command = t.assignments[idx].dest.clone().unwrap_or_default();
        let window_id = t.windows[window].id;
        let full_command = format!("[id=\"{}\"] {}", window_id, command);
        let needs_tree_render = b.parse_command(t, &full_command);
        if needs_tree_render {
            // C: src/assignments.c:68
            needs_render = true;
        }

        /* Prevent crash: if the assigned command included a reload, the
         * assignments array was re-initialized, which will lead to a SEGFAULT
         * if we continue. */
        if old_first_assignment != t.assignments.front().map(|a| a as *const Assignment as usize) {
            break;
        }
    }

    /* C: src/assignments.c:75-77 'if (needs_tree_render) tree_render();'.
     *
     * Rendering here (instead of deferring to the event loop) matters for
     * manage_window(): x_push_changes() updates Con::mapped, and
     * manage_window() checks nc->mapped before con_activate(). With a deferred
     * render an assignment that moves the window to the scratchpad leaves
     * mapped == true, so the window would get the input focus while it is
     * already unmapped (test 233). */
    if needs_render {
        crate::tree::tree_render(t, b);
    }
}

/// C: src/assignments.c:82 `assignment_for`
///
/// Returns the first matching assignment for the given window.
/// (The C code returns an `Assignment *`; this port returns the index into
/// `Tree::assignments`.)
pub fn assignment_for(t: &Tree, window: Option<WindowId>, type_: AssignmentType) -> Option<usize> {
    for (idx, assignment) in t.assignments.iter().enumerate() {
        if (type_ != A_ANY && (assignment.type_ & type_).0 == 0)
            || !match_matches_window(t, &assignment.match_, window)
        {
            continue;
        }
        return Some(idx);
    }

    None
}
