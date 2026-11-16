//! C: src/fake_outputs.c (91 lines) — fake outputs for the testsuite.
//!
//! "Faking outputs is useful in pathological situations (like network X
//! servers which don't support multi-monitor in a useful way) and for our
//! testsuite."
//!
//! The specification is WxH+X+Y, optionally followed by P for a primary
//! output, with multiple outputs separated by commas:
//!   1900x1200+0+0P,1280x1024+1900+0

use std::collections::VecDeque;

use crate::backend::ConBackend;
use crate::data::{output_name, Output, Rect};
use crate::output::output_primary_name;
use crate::randr::{init_ws_for_output, output_init_con};
use crate::tree::Tree;

/// C: src/fake_outputs.c:21 static Output *get_screen_at(unsigned int x,
/// unsigned int y) — the output whose start coordinates are x, y.
fn get_screen_at(t: &Tree, x: u32, y: u32) -> Option<usize> {
    t.outputs
        .iter()
        .position(|output| output.rect.x == x && output.rect.y == y)
}

/// Parses the "%ux%u+%u+%u" prefix of the specification, returning (width,
/// height, x, y, bytes_consumed). C uses sscanf with the %n conversion.
fn parse_geometry(s: &str) -> Option<(u32, u32, u32, u32, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    let number = |i: &mut usize| -> Option<u32> {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == start {
            return None;
        }
        s[start..*i].parse::<u32>().ok()
    };
    let width = number(&mut i)?;
    if i >= b.len() || b[i] != b'x' {
        return None;
    }
    i += 1;
    let height = number(&mut i)?;
    if i >= b.len() || b[i] != b'+' {
        return None;
    }
    i += 1;
    let x = number(&mut i)?;
    if i >= b.len() || b[i] != b'+' {
        return None;
    }
    i += 1;
    let y = number(&mut i)?;
    Some((width, height, x, y, i))
}

/// C: src/fake_outputs.c:38 void fake_outputs_init(const char *output_spec)
pub fn fake_outputs_init(t: &mut Tree, b: &mut dyn ConBackend, output_spec: &str) {
    let mut num_screens = 0usize;
    let mut walk = output_spec;

    while let Some((width, height, x, y, consumed)) = parse_geometry(walk) {
        walk = &walk[consumed..];
        let mut primary = false;
        if walk.starts_with('P') {
            primary = true;
            walk = &walk[1..];
        }
        if walk.starts_with(',') {
            // Skip delimiter
            walk = &walk[1..];
        }

        crate::log::debuglog(&format!(
            "Parsed output as width = {}, height = {} at ({}, {}){}\n",
            width,
            height,
            x,
            y,
            if primary { " (primary)" } else { "" }
        ));

        match get_screen_at(t, x, y) {
            Some(existing) => {
                crate::log::debuglog(&format!("Re-used old output {:?}\n", existing));
                // This screen already exists. We use the littlest screen so
                // that the user can always see the complete workspace.
                t.outputs[existing].rect.width = t.outputs[existing].rect.width.min(width);
                t.outputs[existing].rect.height = t.outputs[existing].rect.height.min(height);
                t.outputs[existing].primary = primary;
            }
            None => {
                let mut names_head = VecDeque::new();
                names_head.push_back(output_name {
                    name: format!("fake-{}", num_screens),
                });
                let new_output = Output {
                    active: true,
                    names_head,
                    rect: Rect {
                        x,
                        y,
                        width,
                        height,
                    },
                    // We always treat the screen at 0x0 as the primary screen
                    primary: x == 0 && y == 0,
                    ..Default::default()
                };
                // C: TAILQ_INSERT_HEAD for 0x0, TAILQ_INSERT_TAIL otherwise
                let idx = if x == 0 && y == 0 {
                    t.outputs.push_front(new_output);
                    0
                } else {
                    t.outputs.push_back(new_output);
                    t.outputs.len() - 1
                };
                crate::log::debuglog(&format!(
                    "Created new fake output {} ({:?})\n",
                    output_primary_name(t, idx),
                    idx
                ));
                output_init_con(t, b, idx);
                init_ws_for_output(t, b, idx);
                num_screens += 1;
                t.outputs[idx].primary = primary;
            }
        }
    }

    if num_screens == 0 {
        crate::log::errorlog("No screens found. Please fix your setup. i3 will exit now.\n");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_geometry;

    #[test]
    fn parses_single_output() {
        assert_eq!(parse_geometry("1024x768+0+0"), Some((1024, 768, 0, 0, 12)));
    }

    #[test]
    fn parses_primary_and_separator() {
        assert_eq!(
            parse_geometry("1900x1200+0+0P,1280x1024+1900+0"),
            Some((1900, 1200, 0, 0, 13))
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_geometry("1024x768"), None);
        assert_eq!(parse_geometry("x+0+0"), None);
    }
}
