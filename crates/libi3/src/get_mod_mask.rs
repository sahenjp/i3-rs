//! C: libi3/get_mod_mask.c

use x11rb::connection::Connection;

/// C: xcb/xcb_keysyms.h xcb_key_symbols_t — the keysym → keycode lookup the C
/// code allocates with xcb_key_symbols_alloc().
///
/// x11rb has no libxcb-keysyms equivalent, so the lookup stays behind this
/// trait: [X11KeySymbols] is the real X server implementation, i3-core passes
/// its xkbcommon-backed keymap in, and the unit tests use a plain table.
pub trait KeySymbols {
    /// C: libi3/get_mod_mask.c:57 xcb_key_symbols_get_keycode(symbols, keysym)
    /// — every keycode bound to the given keysym. An empty list is the C NULL
    /// return (the caller answers 0).
    fn keycodes_for_keysym(&self, keysym: u32) -> Vec<u8>;
}

/// C: libi3/get_mod_mask.c:24 aio_get_mod_mask_for — all-in-one function which
/// returns the modifier mask (XCB_MOD_MASK_*) for the given keysym, for
/// example for XCB_NUM_LOCK (usually configured to mod2).
///
/// This function initiates one round-trip. Use get_mod_mask_for() directly if
/// you already have the modifier mapping and key symbols.
///
/// C takes (keysym, symbols, conn); the port passes the connection first, like
/// the other X11-talking functions of the port.
pub fn aio_get_mod_mask_for(
    conn: &mut dyn ModMaskConn,
    symbols: &dyn KeySymbols,
    keysym: u32,
) -> u32 {
    /* C: 28 xcb_flush(conn) */
    conn.flush();

    /* C: 31-35 — get the current modifier mapping (this is blocking!) */
    match conn.get_modifier_mapping() {
        /* C: uint32_t result = get_mod_mask_for(keysym, symbols, modmap_r); */
        Some(modmap) => get_mod_mask_for(symbols, keysym, &modmap),
        /* C: if (!(modmap_r = ...)) return 0; */
        None => 0,
    }
}

/// C: libi3/get_mod_mask.c:48 get_mod_mask_for — returns the modifier mask
/// (XCB_MOD_MASK_*) for the given keysym, for example for XCB_NUM_LOCK
/// (usually configured to mod2).
///
/// This function does not initiate any round-trips.
pub fn get_mod_mask_for(symbols: &dyn KeySymbols, keysym: u32, modmap: &[u8]) -> u32 {
    /* C: 57-59 — get the list of keycodes for the given symbol */
    let codes = symbols.keycodes_for_keysym(keysym);
    if codes.is_empty() {
        return 0;
    }

    mod_mask_for_keycodes(&codes, modmap)
}

/// C: xcb_get_modifier_mapping_reply_t->keycodes_per_modifier — the reply
/// always describes the 8 modifiers Shift, Lock, Control, Mod1..Mod5.
const MODMAP_MODIFIERS: usize = 8;

/// C: libi3/get_mod_mask.c:61-78 — the inner loops of get_mod_mask_for(): walk
/// the 8 modifiers of the modifier mapping and answer 1 << modifier for the
/// first one that carries one of the resolved keycodes.
///
/// C reaches this loop through xcb_key_symbols_get_keycode(); keeping it
/// separate lets a caller that already resolved the keycodes (or a test) reuse
/// it without a key symbol table.
pub fn mod_mask_for_keycodes(codes: &[u8], modmap: &[u8]) -> u32 {
    /* C iterates modmap_reply->keycodes_per_modifier entries per modifier; a
     * reply shorter than the 8 modifiers cannot describe any of them. */
    if modmap.len() < MODMAP_MODIFIERS {
        return 0;
    }
    let keycodes_per_modifier = modmap.len() / MODMAP_MODIFIERS;

    /* C: 62 — loop through all modifiers (Shift, Lock, Control, Mod1-Mod5) */
    for modifier in 0..MODMAP_MODIFIERS {
        for j in 0..keycodes_per_modifier {
            /* C: 65 — store the current keycode (for modifier 'modifier') */
            let mod_code = modmap[modifier * keycodes_per_modifier + j];
            /* XCB_NO_SYMBOL: the padding slots of a short modifier map. C
             * compares them against the keycode list and never matches. */
            if mod_code == 0 {
                continue;
            }
            /* C: 68-71 — is that keycode in the list of resolved keycodes? */
            if codes.contains(&mod_code) {
                /* C: 73-75 — this corresponds to the XCB_MOD_MASK_* constants */
                return 1 << modifier;
            }
        }
    }

    0
}

/// C: the xcb_connection_t requests of libi3/get_mod_mask.c.
pub trait ModMaskConn {
    /// C: 28 xcb_flush(conn)
    fn flush(&mut self);
    /// C: 31-32 xcb_get_modifier_mapping + xcb_get_modifier_mapping_reply —
    /// the 8 * keycodes_per_modifier keycodes in Shift/Lock/Control/Mod1..Mod5
    /// order. None is the NULL reply C answers 0 for.
    fn get_modifier_mapping(&mut self) -> Option<Vec<u8>>;
}

/// C: the same two requests over the real x11rb connection (taken by shared
/// reference: every x11rb request only needs one).
impl<C> ModMaskConn for &C
where
    C: Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
{
    fn flush(&mut self) {
        /* C: xcb_flush */
        let _ = Connection::flush(*self);
    }

    fn get_modifier_mapping(&mut self) -> Option<Vec<u8>> {
        let reply = x11rb::protocol::xproto::ConnectionExt::get_modifier_mapping(*self)
            .ok()?
            .reply()
            .ok()?;
        Some(reply.keycodes)
    }
}

/// C: xcb_key_symbols_alloc(conn) — the real X server's key symbols.
///
/// C's xcb_key_symbols_t keeps the keyboard mapping cached; the port reads it
/// once with a single GetKeyboardMapping round trip and answers the keysym →
/// keycodes question from that table. Unlike libxcb-keysyms this does not
/// apply the case pairing (a lower-case keysym also matching its upper-case
/// keycode); i3 only asks for XCB_NUM_LOCK, which has no case pair.
pub struct X11KeySymbols {
    /// (keycode, keysyms of all groups/levels), in keycode order.
    keys: Vec<(u8, Vec<u32>)>,
}

impl X11KeySymbols {
    /// C: xcb_key_symbols_alloc(conn) — one round trip.
    pub fn new<C>(conn: &C) -> Option<X11KeySymbols>
    where
        C: Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
    {
        let setup = conn.setup();
        let count = setup.max_keycode.saturating_sub(setup.min_keycode) + 1;
        let reply = x11rb::protocol::xproto::ConnectionExt::get_keyboard_mapping(
            conn,
            setup.min_keycode,
            count,
        )
        .ok()?
        .reply()
        .ok()?;
        let keysyms_per_keycode = reply.keysyms_per_keycode as usize;
        if keysyms_per_keycode == 0 {
            return None;
        }
        let keys = reply
            .keysyms
            .chunks(keysyms_per_keycode)
            .enumerate()
            .map(|(index, syms)| (setup.min_keycode.wrapping_add(index as u8), syms.to_vec()))
            .collect();
        Some(X11KeySymbols { keys })
    }
}

impl KeySymbols for X11KeySymbols {
    fn keycodes_for_keysym(&self, keysym: u32) -> Vec<u8> {
        let mut codes = Vec::new();
        for (keycode, syms) in &self.keys {
            if syms.contains(&keysym) && !codes.contains(keycode) {
                codes.push(*keycode);
            }
        }
        codes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keysym → keycodes answer of xcb_key_symbols_get_keycode().
    struct FixedSymbols {
        keys: Vec<(u32, Vec<u8>)>,
    }

    impl FixedSymbols {
        fn new(keys: &[(u32, &[u8])]) -> FixedSymbols {
            FixedSymbols {
                keys: keys.iter().map(|(s, c)| (*s, c.to_vec())).collect(),
            }
        }
    }

    impl KeySymbols for FixedSymbols {
        fn keycodes_for_keysym(&self, keysym: u32) -> Vec<u8> {
            self.keys
                .iter()
                .find(|(s, _)| *s == keysym)
                .map(|(_, c)| c.clone())
                .unwrap_or_default()
        }
    }

    /// A conn that records the two requests C issues.
    #[derive(Default)]
    struct StubConn {
        modmap: Option<Vec<u8>>,
        log: Vec<String>,
    }

    impl ModMaskConn for StubConn {
        fn flush(&mut self) {
            self.log.push("flush".to_string());
        }

        fn get_modifier_mapping(&mut self) -> Option<Vec<u8>> {
            self.log.push("get_modifier_mapping".to_string());
            self.modmap.clone()
        }
    }

    /// C: libi3/get_mod_mask.c:61-78 — Num_Lock lives on Mod2, so the mask is
    /// 1 << 4.
    #[test]
    fn mod_mask_for_keycodes_walks_the_eight_modifiers() {
        // 8 modifiers x 4 keycodes, like the reply of a normal X server.
        let mut modmap = vec![0u8; 32];
        modmap[0] = 50; // Shift
        modmap[4 * 4] = 77; // Mod2 = Num_Lock
        modmap[7 * 4 + 1] = 90; // Mod5
        assert_eq!(mod_mask_for_keycodes(&[77], &modmap), 1 << 4);
        assert_eq!(mod_mask_for_keycodes(&[50], &modmap), 1 << 0);
        assert_eq!(mod_mask_for_keycodes(&[90], &modmap), 1 << 7);
        // A keycode that is not a modifier answers 0.
        assert_eq!(mod_mask_for_keycodes(&[9], &modmap), 0);
        // The first matching modifier wins (C returns immediately).
        assert_eq!(mod_mask_for_keycodes(&[50, 77], &modmap), 1 << 0);
        // A reply shorter than the 8 modifiers describes none of them.
        assert_eq!(mod_mask_for_keycodes(&[77], &[]), 0);
    }

    /// C: libi3/get_mod_mask.c:48 — the keysym is resolved first; an unknown
    /// keysym answers 0 without touching the modifier map.
    #[test]
    fn get_mod_mask_for_resolves_the_keysym_first() {
        let symbols = FixedSymbols::new(&[(0xff7f, &[77])]);
        let mut modmap = vec![0u8; 32];
        modmap[4 * 4] = 77;
        assert_eq!(get_mod_mask_for(&symbols, 0xff7f, &modmap), 1 << 4);
        // C: xcb_key_symbols_get_keycode returns NULL -> return 0.
        assert_eq!(get_mod_mask_for(&symbols, 0x61, &modmap), 0);
    }

    /// C: libi3/get_mod_mask.c:24 — one flush + one GetModifierMapping
    /// round-trip, then the same answer as get_mod_mask_for().
    #[test]
    fn aio_get_mod_mask_for_flushes_then_reads_the_mapping() {
        let symbols = FixedSymbols::new(&[(0xff7f, &[77])]);
        let mut modmap = vec![0u8; 32];
        modmap[4 * 4] = 77;
        let mut conn = StubConn {
            modmap: Some(modmap),
            log: Vec::new(),
        };
        assert_eq!(aio_get_mod_mask_for(&mut conn, &symbols, 0xff7f), 1 << 4);
        assert_eq!(conn.log, vec!["flush", "get_modifier_mapping"]);

        // C: a NULL modifier mapping reply answers 0.
        let mut failing = StubConn {
            modmap: None,
            log: Vec::new(),
        };
        assert_eq!(aio_get_mod_mask_for(&mut failing, &symbols, 0xff7f), 0);
        assert_eq!(failing.log, vec!["flush", "get_modifier_mapping"]);
    }

    /// Real X (headless Xvfb): xcb_key_symbols_alloc + both requests against a
    /// real server must find Num_Lock on mod2, exactly like C.
    #[test]
    fn real_x_numlock_is_on_mod2() {
        let Some(xvfb) = crate::test_util::start_xvfb() else {
            eprintln!("skipping: no Xvfb available");
            return;
        };
        let (conn, _screen) = match x11rb::connect(Some(xvfb.display())) {
            Ok(conn) => conn,
            Err(err) => panic!("could not connect to {}: {err}", xvfb.display()),
        };
        let symbols = X11KeySymbols::new(&conn).expect("GetKeyboardMapping failed");
        // C: xcb_key_symbols_get_keycode(XCB_NUM_LOCK) is non-empty on a
        // standard keymap.
        assert!(
            !symbols.keycodes_for_keysym(0xff7f).is_empty(),
            "no keycode for Num_Lock"
        );
        let mut conn_ref = &conn;
        let mask = aio_get_mod_mask_for(&mut conn_ref, &symbols, 0xff7f);
        assert_eq!(mask, 1 << 4, "Num_Lock is expected on mod2");

        // get_mod_mask_for() must agree with the round-trip version.
        let modmap = x11rb::protocol::xproto::ConnectionExt::get_modifier_mapping(&conn)
            .expect("get_modifier_mapping failed")
            .reply()
            .expect("get_modifier_mapping reply failed")
            .keycodes;
        assert_eq!(get_mod_mask_for(&symbols, 0xff7f, &modmap), mask);
    }
}
