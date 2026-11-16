//! Invariant tests for the i3-core tree layer.
//!
//! Each test asserts an invariant that also holds in the C implementation
//! (i3 4.25, commit 8a97b0f). Test layout mirrors the C test harness:
//! `tree_init` + `workspace_get` on the __i3 pseudo-output, NullBackend.
use i3_core::*;

/// C: src/tree.c:126 `tree_init` creates root + __i3 pseudo-output.
fn setup() -> (Tree, backend::NullBackend) {
    let mut t = Tree::new();
    let mut b = backend::NullBackend;
    // Mirror a user config with `default_orientation horizontal` so that
    // workspace_get produces an L_SPLITH workspace (C:
    // src/workspace.c:_workspace_apply_default_orientation with rect 0x0
    // would pick vertical otherwise).
    t.config.default_orientation = data::Orientation::HORIZ;
    tree::tree_init(&mut t, &mut b, Rect { x: 0, y: 0, width: 1280, height: 1024 });
    (t, b)
}

fn find_by_name(t: &Tree, name: &str) -> Option<ConId> {
    t.all_cons
        .iter()
        .copied()
        .find(|&c| t.cons[c].name.as_deref() == Some(name))
}

/// tree_init + a regular workspace "0" on the __i3 output.
fn setup_ws() -> (Tree, backend::NullBackend, ConId) {
    let (mut t, mut b) = setup();
    let scratch = find_by_name(&t, "__i3_scratch").expect("__i3_scratch exists");
    con::con_focus(&mut t, &mut b, scratch);
    let ws = workspace::workspace_get(&mut t, &mut b, "0");
    // Make ws the visible workspace (C: workspace_show marks the visible
    // workspace with fullscreen_mode == CF_OUTPUT; con_fullscreen_permits_focusing
    // relies on this invariant when walking up from `focused`).
    workspace::workspace_show(&mut t, &mut b, ws);
    (t, b, ws)
}

fn nodes(t: &Tree, con: ConId) -> Vec<ConId> {
    t.cons[con].nodes_head.iter().copied().collect()
}

fn focus(t: &Tree, con: ConId) -> Vec<ConId> {
    t.cons[con].focus_head.iter().copied().collect()
}

/// Invariant: every child of a con is in exactly one of nodes_head /
/// floating_head, and always also in the parent's focus_head
/// (C: con.c _con_attach / con_detach).
fn assert_children_registered(t: &Tree) {
    for &con in t.all_cons.iter() {
        for &child in t.cons[con].nodes_head.iter() {
            assert_eq!(t.cons[child].parent, Some(con));
            assert!(t.cons[con].focus_head.contains(&child));
        }
        for &child in t.cons[con].floating_head.iter() {
            assert_eq!(t.cons[child].parent, Some(con));
            assert!(t.cons[con].focus_head.contains(&child));
        }
    }
}

/// (1) con_attach: nodes order is insertion order, focus_head starts as the
/// same order (tail insert) and becomes most-recently-focused-first after
/// con_focus (C: src/con.c:124 _con_attach, src/con.c:262 con_focus).
#[test]
fn con_attach_nodes_and_focus_order() {
    let (mut t, mut b, ws) = setup_ws();
    // i3 focuses each newly managed window right after opening it
    // (C: src/manage.c calls tree_open_con followed by con_focus).
    let a = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, a);
    let b2 = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, b2);
    let c = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, c);

    // _con_attach inserts after the currently focused tiling con, so nodes
    // order is the opening order (C: src/con.c:124 _con_attach).
    assert_eq!(nodes(&t, ws), vec![a, b2, c]);
    // focus_head: most recently focused first (C: src/con.c:262 con_focus).
    assert_eq!(focus(&t, ws), vec![c, b2, a]);
    assert_eq!(t.focused, Some(c));
    assert_children_registered(&t);
}

/// (2) tree_split keeps the sibling percent sum at 1.0 after
/// con_fix_percent (C: src/tree.c:313 tree_split, src/con.c:1131
/// con_fix_percent).
#[test]
fn tree_split_then_con_fix_percent_sums_to_one() {
    let (mut t, mut b, ws) = setup_ws();
    let a = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, a);
    let sib = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, sib);

    tree::tree_split(&mut t, &mut b, a, data::Orientation::VERT);
    let newp = t.cons[a].parent.expect("a has a parent after tree_split");
    assert_ne!(newp, ws);
    assert!(nodes(&t, newp).contains(&a));
    assert!(focus(&t, ws).contains(&newp));

    // A child without a percent set must be normalized into the sum (C keeps
    // the running-total bug, but the final sum is still 1.0).
    let third = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, third);
    t.cons[third].percent = 0.0;
    con::con_fix_percent(&mut t, ws);
    // tree_split leaves the moved child with percent == 0 until the next
    // con_fix_percent (C: src/tree.c:361-364 + con_fix_percent call sites).
    con::con_fix_percent(&mut t, newp);

    let sum: f64 = nodes(&t, ws).iter().map(|&c| t.cons[c].percent).sum();
    assert!((sum - 1.0).abs() < 1e-9, "ws sibling sum was {}", sum);

    let inner_sum: f64 = nodes(&t, newp).iter().map(|&c| t.cons[c].percent).sum();
    assert!((inner_sum - 1.0).abs() < 1e-9, "split parent sum was {}", inner_sum);
    assert_children_registered(&t);
}

/// (3) tree_close_internal removes the whole subtree and focuses
/// con_next_focused (computed before detaching; C: src/tree.c:175
/// tree_close_internal, src/con.c:1524 con_next_focused).
#[test]
fn tree_close_internal_removes_children_and_focuses_next_focused() {
    let (mut t, mut b, ws) = setup_ws();
    let a = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, a);
    let bc = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, bc);
    let c = tree::tree_open_con(&mut t, &mut b, Some(bc), None);
    con::con_focus(&mut t, &mut b, c);
    assert_eq!(t.focused, Some(c));

    let closed = tree::tree_close_internal(
        &mut t,
        &mut b,
        bc,
        data::KillWindow::DONT_KILL_WINDOW,
        false,
    );
    assert!(closed);

    // The whole subtree vanished from the backing store.
    assert!(t.cons.get(bc).is_none(), "bc still present");
    assert!(t.cons.get(c).is_none(), "c still present");
    assert_eq!(nodes(&t, ws), vec![a]);

    // con_next_focused(bc) = a: first entry of ws's focus stack that is not bc
    // (focus was [bc, a] with c descended under bc).
    assert_eq!(t.focused, Some(a));
    assert_children_registered(&t);
}

/// (4) workspace_get resolves numbered workspaces in num order and named
/// workspaces keep num == -1 (C: src/con.c:124 _con_attach workspace branch,
/// src/workspace.c:97 workspace_get).
#[test]
fn workspace_get_orders_by_num_named_ws_is_minus_one() {
    let (mut t, mut b) = setup();
    let scratch = find_by_name(&t, "__i3_scratch").expect("__i3_scratch exists");
    con::con_focus(&mut t, &mut b, scratch);

    let w2 = workspace::workspace_get(&mut t, &mut b, "2");
    let w1 = workspace::workspace_get(&mut t, &mut b, "1");
    let web = workspace::workspace_get(&mut t, &mut b, "web");

    assert_eq!(t.cons[w2].num, 2);
    assert_eq!(t.cons[w1].num, 1);
    assert_eq!(t.cons[web].num, -1);

    // Getting "1" again returns the existing workspace.
    assert_eq!(workspace::workspace_get(&mut t, &mut b, "1"), w1);

    let content = t.cons[w1].parent.expect("workspace has a content parent");
    let order: Vec<i32> = nodes(&t, content).iter().map(|&c| t.cons[c].num).collect();
    // ["1", "2", __i3_scratch(-1), "web"(-1)]: numbered ascending, named at
    // the tail (C inserts num == -1 at the tail of the content container).
    assert_eq!(order, vec![1, 2, -1, -1]);
    assert_eq!(nodes(&t, content)[3], web);
}

/// (5) The __i3 pseudo-output is the first entry of the root's nodes_head
/// (C: src/tree.c:27 _create___i3 attaches it before any real output).
#[test]
fn i3_pseudo_output_is_first_in_root() {
    let (t, _b) = setup();
    let croot = t.croot.expect("tree_init created croot");
    let first = t.cons[croot].nodes_head[0];
    assert_eq!(t.cons[first].name.as_deref(), Some("__i3"));
    assert_eq!(t.cons[first].r#type, ConType::CT_OUTPUT);
}

/// (6) At most one CF_OUTPUT fullscreen container per workspace: enabling
/// fullscreen on a second con disables the previous one (C:
/// src/con.c:1234 con_enable_fullscreen + src/con.c:598 con_get_fullscreen_con).
#[test]
fn only_one_cf_output_fullscreen_per_workspace() {
    let (mut t, mut b, ws) = setup_ws();
    let a = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, a);
    let c = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, c);

    con::con_focus(&mut t, &mut b, a);
    con::con_enable_fullscreen(&mut t, &mut b, a, FullscreenMode::CF_OUTPUT);
    assert_eq!(t.cons[a].fullscreen_mode, FullscreenMode::CF_OUTPUT);
    assert_eq!(
        con::con_get_fullscreen_con(&t, ws, FullscreenMode::CF_OUTPUT),
        Some(a)
    );

    con::con_enable_fullscreen(&mut t, &mut b, c, FullscreenMode::CF_OUTPUT);
    assert_eq!(t.cons[a].fullscreen_mode, FullscreenMode::CF_NONE);
    assert_eq!(
        con::con_get_fullscreen_con(&t, ws, FullscreenMode::CF_OUTPUT),
        Some(c)
    );
}

/// (7) tree_next honours focus_wrapping: OFF stops at the workspace edge,
/// ON / FORCE / WORKSPACE wrap to the first sibling (C: src/tree.c:503
/// get_tree_next). With no real outputs registered, the workspace-switch
/// fallback finds no next workspace, mirroring C's
/// get_output_containing() == NULL.
#[test]
fn tree_next_respects_focus_wrapping() {
    let (mut t, mut b, ws) = setup_ws();
    let a = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, a);
    let mid = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, mid);
    let last = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, last);

    // Regular move: next sibling, independent of the wrapping mode.
    con::con_focus(&mut t, &mut b, a);
    t.config.focus_wrapping = data::FocusWrapping::FOCUS_WRAPPING_OFF;
    tree::tree_next(&mut t, &mut b, a, Direction::D_RIGHT);
    assert_eq!(t.focused, Some(mid));

    // OFF: at the last child nothing changes (no next output registered).
    con::con_focus(&mut t, &mut b, last);
    tree::tree_next(&mut t, &mut b, last, Direction::D_RIGHT);
    assert_eq!(t.focused, Some(last), "FOCUS_WRAPPING_OFF must not wrap");

    // ON: wraps to the first sibling of the workspace.
    t.config.focus_wrapping = data::FocusWrapping::FOCUS_WRAPPING_ON;
    tree::tree_next(&mut t, &mut b, last, Direction::D_RIGHT);
    assert_eq!(t.focused, Some(a), "FOCUS_WRAPPING_ON must wrap to first");

    // FORCE: always returns a target within the parent.
    con::con_focus(&mut t, &mut b, last);
    t.config.focus_wrapping = data::FocusWrapping::FOCUS_WRAPPING_FORCE;
    tree::tree_next(&mut t, &mut b, last, Direction::D_RIGHT);
    assert_eq!(t.focused, Some(a), "FOCUS_WRAPPING_FORCE must wrap to first");

    // WORKSPACE: wraps within the workspace like ON.
    con::con_focus(&mut t, &mut b, last);
    t.config.focus_wrapping = data::FocusWrapping::FOCUS_WRAPPING_WORKSPACE;
    tree::tree_next(&mut t, &mut b, last, Direction::D_RIGHT);
    assert_eq!(t.focused, Some(a), "FOCUS_WRAPPING_WORKSPACE must wrap");

    // BACKWARDS from the first child with ON wraps to the last sibling.
    con::con_focus(&mut t, &mut b, a);
    t.config.focus_wrapping = data::FocusWrapping::FOCUS_WRAPPING_ON;
    tree::tree_next(&mut t, &mut b, a, Direction::D_LEFT);
    assert_eq!(t.focused, Some(last));
}

/// (8) con_descend_focused follows the focus stack (C: src/con.c:1680
/// con_descend_focused).
#[test]
fn con_descend_focused_follows_focus_stack() {
    let (mut t, mut b, ws) = setup_ws();
    let a = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, a);
    let c = tree::tree_open_con(&mut t, &mut b, Some(ws), None);
    con::con_focus(&mut t, &mut b, c);

    assert_eq!(con::con_descend_focused(&t, ws), c);

    let grand = tree::tree_open_con(&mut t, &mut b, Some(c), None);
    con::con_focus(&mut t, &mut b, grand);
    assert_eq!(con::con_descend_focused(&t, ws), grand);

    con::con_focus(&mut t, &mut b, a);
    assert_eq!(con::con_descend_focused(&t, ws), a);
}
