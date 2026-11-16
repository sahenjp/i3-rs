#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! C: i3bar/include/common.h — the data structures shared by all i3bar
//! modules (rect, blockalign_t, status_block, statusline_head).

use libi3::string::{i3string_as_utf8, i3string_get_num_bytes, i3string_is_markup, i3string_set_markup, i3String};

/// C: include/common.h:29 typedef struct rect_t rect
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// C: include/common.h:37 typedef enum { ALIGN_LEFT, ALIGN_CENTER, ALIGN_RIGHT } blockalign_t
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum blockalign_t {
    /// C: ALIGN_LEFT — first value, so it is the default.
    #[default]
    ALIGN_LEFT = 0,
    ALIGN_CENTER = 1,
    ALIGN_RIGHT = 2,
}

/// C: include/common.h:45 struct status_block_render_desc
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct status_block_render_desc {
    pub width: u32,
    pub x_offset: u32,
    pub x_append: u32,
}

/// C: include/common.h:55 struct status_block — one JSON dictionary of the
/// status line.
///
/// (No Debug: libi3's i3String is opaque and does not implement it.)
#[derive(Default)]
pub struct status_block {
    pub full_text: Option<i3String>,
    pub short_text: Option<i3String>,

    pub use_short: bool,
    pub render_length: u32,

    pub color: Option<String>,
    pub background: Option<String>,
    pub border: Option<String>,

    /// C: uint32_t min_width — numeric value, or the measured text width of
    /// min_width_str.
    pub min_width: u32,
    pub min_width_str: Option<String>,

    pub align: blockalign_t,

    pub urgent: bool,
    pub no_separator: bool,
    pub border_top: u32,
    pub border_right: u32,
    pub border_bottom: u32,
    pub border_left: u32,
    pub pango_markup: bool,

    /// C: uint32_t sep_block_width — pixels needed to render a separator.
    pub sep_block_width: u32,

    /// C: struct status_block_render_desc full_render
    pub full_render: status_block_render_desc,
    /// C: struct status_block_render_desc short_render
    pub short_render: status_block_render_desc,

    pub name: Option<String>,
    pub instance: Option<String>,
}

impl status_block {
    /// C: child.c:151 copy_statusline() — the C code memcpy()s the struct, so
    /// the i3String pointers are shared between the two lists. In Rust the
    /// text is deep-copied (i3string_copy()) instead, which is the only
    /// difference and is not observable: the C code never mutates the copied
    /// text in place.
    pub fn deep_copy(&mut self) -> status_block {
        status_block {
            full_text: copy_i3string(&mut self.full_text),
            short_text: copy_i3string(&mut self.short_text),
            use_short: self.use_short,
            render_length: self.render_length,
            color: self.color.clone(),
            background: self.background.clone(),
            border: self.border.clone(),
            min_width: self.min_width,
            min_width_str: self.min_width_str.clone(),
            align: self.align,
            urgent: self.urgent,
            no_separator: self.no_separator,
            border_top: self.border_top,
            border_right: self.border_right,
            border_bottom: self.border_bottom,
            border_left: self.border_left,
            pango_markup: self.pango_markup,
            sep_block_width: self.sep_block_width,
            full_render: self.full_render,
            short_render: self.short_render,
            name: self.name.clone(),
            instance: self.instance.clone(),
        }
    }

    /// C: i3string_get_num_bytes(block->full_text) — the C code dereferences
    /// full_text unconditionally; stdin_end_map() guarantees it is non-NULL.
    pub fn full_text_num_bytes(&mut self) -> usize {
        match self.full_text.as_mut() {
            Some(t) => i3string_get_num_bytes(t),
            None => 0,
        }
    }

    /// Convenience for tests and DLOG output: the UTF-8 text of full_text.
    pub fn full_text_utf8(&mut self) -> Option<String> {
        self.full_text.as_mut().and_then(|t| i3string_as_utf8(t)).map(|s| s.to_string())
    }

    /// Convenience for tests and DLOG output: the UTF-8 text of short_text.
    pub fn short_text_utf8(&mut self) -> Option<String> {
        self.short_text.as_mut().and_then(|t| i3string_as_utf8(t)).map(|s| s.to_string())
    }

    /// C: child.c:441-447 — i3string_set_markup(full_text/short_text).
    pub fn set_markup(&mut self, pango_markup: bool) {
        if let Some(t) = self.full_text.as_mut() {
            i3string_set_markup(t, pango_markup);
        }
        if let Some(t) = self.short_text.as_mut() {
            i3string_set_markup(t, pango_markup);
        }
    }
}

/// C: child.c:163-168 — I3STRING_FREE(first->full_text) in clear_statusline():
/// the i3String is freed, the pointer would be NULLed.
fn copy_i3string(s: &mut Option<i3String>) -> Option<i3String> {
    let text = s.as_mut()?;
    let markup = i3string_is_markup(text);
    let utf8 = i3string_as_utf8(text).unwrap_or("").to_string();
    let mut copy = libi3::i3string_from_utf8(&utf8);
    i3string_set_markup(&mut copy, markup);
    Some(copy)
}

/// The statusline the bar renders.
///
/// C: include/common.h:94 extern TAILQ_HEAD(statusline_head, status_block) statusline_head
/// (a file-global TAILQ in child.c). The Rust port keeps it in a thread-local
/// [std::cell::RefCell] so that unit tests are isolated from each other while
/// the ported code keeps its global call shape.
pub mod statusline {
    use super::status_block;
    use std::cell::RefCell;

    thread_local! {
        /// C: child.c:89 struct statusline_head statusline_head
        static HEAD: RefCell<Vec<status_block>> = const { RefCell::new(Vec::new()) };
        /// C: child.c:91 struct statusline_head statusline_buffer — used
        /// temporarily while reading a statusline.
        static BUFFER: RefCell<Vec<status_block>> = const { RefCell::new(Vec::new()) };
    }

    pub fn head<R>(f: impl FnOnce(&mut Vec<status_block>) -> R) -> R {
        HEAD.with(|h| f(&mut h.borrow_mut()))
    }

    pub fn buffer<R>(f: impl FnOnce(&mut Vec<status_block>) -> R) -> R {
        BUFFER.with(|h| f(&mut h.borrow_mut()))
    }

    /// Drop both lists (test helper; the C process never needs this).
    pub fn reset() {
        HEAD.with(|h| h.borrow_mut().clear());
        BUFFER.with(|h| h.borrow_mut().clear());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_copy_copies_text_and_fields() {
        let mut block = status_block {
            full_text: Some(libi3::i3string_from_utf8("hello")),
            short_text: Some(libi3::i3string_from_utf8("h")),
            name: Some("n".to_string()),
            min_width: 42,
            ..Default::default()
        };
        let mut copy = block.deep_copy();
        assert_eq!(copy.full_text_utf8(), Some("hello".to_string()));
        assert_eq!(copy.name.as_deref(), Some("n"));
        assert_eq!(copy.min_width, 42);
        assert_eq!(copy.short_text_utf8(), Some("h".to_string()));
    }

    #[test]
    fn full_text_num_bytes_counts_bytes() {
        let mut block = status_block {
            full_text: Some(libi3::i3string_from_utf8("héllo")),
            ..Default::default()
        };
        assert_eq!(block.full_text_num_bytes(), 6);
    }

    #[test]
    fn set_markup_sets_both_strings() {
        let mut block = status_block {
            full_text: Some(libi3::i3string_from_utf8("a")),
            short_text: Some(libi3::i3string_from_utf8("b")),
            ..Default::default()
        };
        block.set_markup(true);
        assert!(libi3::i3string_is_markup(block.full_text.as_ref().unwrap()));
        assert!(libi3::i3string_is_markup(block.short_text.as_ref().unwrap()));
    }
}
