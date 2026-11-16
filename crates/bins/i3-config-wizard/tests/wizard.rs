//! The state-machine behaviour of i3-config-wizard, compared against
//! C: i3-config-wizard/main.c.
//!
//! The generated config is checked byte for byte against the rules of C: the
//! template is the real etc/config.keycodes of the C source tree, the keymap is
//! a fixed US layout, and the drawing calls are recorded instead of being sent
//! to cairo.

use i3_config_wizard::{
    binding_parser, col_x, finish, handle_button_press, handle_expose, handle_key_press,
    keysym_name, row_y, KeyOutcome, Keymap, Modifier, Step, Wizard, WizardRenderer,
    GENERATED_HEADER,
};
use libi3::draw_util::color_t;
use libi3::string::{i3string_as_utf8, i3String};

/// C: xcb_key_symbols / xkb_keymap — a fixed table of the US layout keycodes
/// that etc/config.keycodes uses.
struct FakeKeymap {
    keys: Vec<(u8, u32, u32)>,
}

impl FakeKeymap {
    fn us() -> FakeKeymap {
        let mut keys: Vec<(u8, u32, u32)> = Vec::new();
        let mut add_row = |first: u8, unshifted: &str, shifted: &str| {
            for (i, c) in unshifted.chars().enumerate() {
                let keycode = first + i as u8;
                let upper = shifted.chars().nth(i).unwrap_or(c);
                keys.push((keycode, c as u32, upper as u32));
            }
        };
        // C: evdev keycodes = X keycode - 8, so 10..19 are the digit row.
        add_row(10, "1234567890", "!@#$%^&*()");
        add_row(24, "qwertyuiop", "QWERTYUIOP");
        add_row(38, "asdfghjkl;", "ASDFGHJKL:");
        add_row(52, "zxcvbnm,./", "ZXCVBNM<>?");
        keys.push((20, b'-' as u32, b'_' as u32));
        keys.push((36, 0xff0d, 0xff0d)); // Return
        keys.push((9, 0xff1b, 0xff1b)); // Escape
        keys.push((65, 0x20, 0x20)); // space
        keys.push((111, 0xff52, 0xff52)); // Up
        keys.push((113, 0xff51, 0xff51)); // Left
        keys.push((114, 0xff53, 0xff53)); // Right
        keys.push((116, 0xff54, 0xff54)); // Down
        keys.push((50, 0xffe1, 0xffe1)); // Shift_L
        keys.push((37, 0xffe3, 0xffe3)); // Control_L
        keys.push((64, 0xffe9, 0xffe9)); // Alt_L
        keys.push((133, 0xffeb, 0xffeb)); // Super_L
        FakeKeymap { keys }
    }
}

impl Keymap for FakeKeymap {
    fn min_keycode(&self) -> u8 {
        8
    }
    fn max_keycode(&self) -> u8 {
        255
    }
    fn key_get_syms_by_level(&mut self, keycode: u8, level: i32) -> Vec<u32> {
        let entry = self.keys.iter().find(|k| k.0 == keycode);
        match entry {
            Some((_, level0, level1)) => {
                if level == 0 {
                    vec![*level0]
                } else {
                    vec![*level1]
                }
            }
            None => Vec::new(),
        }
    }
    fn keysym_get_name(&mut self, sym: u32) -> Option<String> {
        keysym_name(sym)
    }
    fn key_symbols_get_keysym(&mut self, keycode: u8, level: i32) -> u32 {
        // Layers 0/2 are the unshifted ones, 1/3 the shifted ones (group 0).
        let wanted_level = level % 2;
        match self.keys.iter().find(|k| k.0 == keycode) {
            Some((_, level0, level1)) => {
                if wanted_level == 0 {
                    *level0
                } else {
                    *level1
                }
            }
            None => 0,
        }
    }
    fn key_press_lookup_keysym(&mut self, keycode: u8, col: i32) -> u32 {
        self.key_symbols_get_keysym(keycode, col)
    }
}

#[derive(Debug, PartialEq)]
enum Op {
    Clear(&'static str),
    Bold(bool),
    Text { text: String, x: i32, y: i32, max_width: i32 },
    Title(String),
    Flush,
}

struct RecordingRenderer {
    ops: Vec<Op>,
    pub char_width: i32,
    pub font_height: i32,
    pub dpi_scale: i32,
}

impl RecordingRenderer {
    fn new() -> RecordingRenderer {
        RecordingRenderer {
            ops: Vec::new(),
            char_width: 8,
            font_height: 16,
            dpi_scale: 1,
        }
    }
}

fn color_of(name: &str) -> color_t {
    let mut c = color_t::default();
    c.colorpixel = match name {
        "#000000" => 0x000000,
        "#FFFFFF" => 0xffffff,
        "#00FF00" => 0x00ff00,
        "#FF0000" => 0xff0000,
        _ => 0,
    };
    c
}

impl WizardRenderer for RecordingRenderer {
    fn logical_px(&mut self, logical: i32) -> i32 {
        logical * self.dpi_scale
    }
    fn hex_to_color(&mut self, color: &str) -> color_t {
        color_of(color)
    }
    fn clear_surface(&mut self, color: color_t) {
        let name = if color.colorpixel == 0x000000 { "#000000" } else { "#FFFFFF" };
        self.ops.push(Op::Clear(name));
    }
    fn text(
        &mut self,
        text: &mut i3String,
        _fg_color: color_t,
        _bg_color: color_t,
        x: i32,
        y: i32,
        max_width: i32,
    ) {
        let s = i3string_as_utf8(text).unwrap_or("").to_string();
        self.ops.push(Op::Text { text: s, x, y, max_width });
    }
    fn set_font_bold(&mut self, bold: bool) {
        self.ops.push(Op::Bold(bold));
    }
    fn font_height(&self) -> i32 {
        self.font_height
    }
    fn char_width(&self) -> i32 {
        self.char_width
    }
    fn flush(&mut self) {
        self.ops.push(Op::Flush);
    }
    fn set_window_title(&mut self, title: &str) {
        self.ops.push(Op::Title(title.to_string()));
    }
}

fn wizard_for_welcome() -> Wizard {
    let mut wizard = Wizard::default();
    wizard.config_path = "/home/u/.config/i3/config".to_string();
    wizard
}

fn rewrite_line(line: &str) -> String {
    let mut parser = binding_parser::default();
    let mut keymap = FakeKeymap::us();
    parser.rewrite_binding(line, &mut keymap).expect("binding")
}

#[test]
fn rewrite_binding_turns_keycodes_into_keysyms() {
    // C: main.c:257-311 — the __CALL branch builds "bindsym <mods><keysym>
    // <command>". The expectations are the lines a real i3 config.keycodes
    // produces.
    assert_eq!(
        rewrite_line("bindcode $mod+36 exec i3-sensible-terminal\n"),
        "bindsym $mod+Return exec i3-sensible-terminal\n"
    );
    // Shift is dropped when the lower-case symbol is unambiguous.
    assert_eq!(
        rewrite_line("bindcode $mod+Shift+24 kill\n"),
        "bindsym $mod+Shift+q kill\n"
    );
    // Not a letter: X11 keysym names for punctuation.
    assert_eq!(
        rewrite_line("bindcode $mod+Shift+20 move scratchpad\n"),
        "bindsym $mod+Shift+minus move scratchpad\n"
    );
    assert_eq!(
        rewrite_line("bindcode $mod+Shift+65 floating toggle\n"),
        "bindsym $mod+Shift+space floating toggle\n"
    );
    assert_eq!(
        rewrite_line("bindcode $mod+10 workspace number $ws1\n"),
        "bindsym $mod+1 workspace number $ws1\n"
    );
    assert_eq!(
        rewrite_line("bindcode $mod+113 focus left\n"),
        "bindsym $mod+Left focus left\n"
    );
    // Without modifiers the "+" is omitted.
    assert_eq!(
        rewrite_line("        bindcode 44 resize shrink width 10 px or 10 ppt\n"),
        "bindsym j resize shrink width 10 px or 10 ppt\n"
    );
    // A quoted command string keeps its quotes.
    assert_eq!(
        rewrite_line("bindcode $mod+Shift+26 exec \"i3-nagbar -t warning\"\n"),
        "bindsym $mod+Shift+e exec \"i3-nagbar -t warning\"\n"
    );
}

#[test]
fn finish_writes_the_c_header_and_rewrites_every_bindcode() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // The template is vendored (third_party/i3/etc/config.keycodes, BSD-3-Clause);
    // a full i3 source tree next to the checkout still wins when present.
    let vendored = manifest_dir.join("../../../third_party/i3/etc/config.keycodes");
    let from_src = manifest_dir.join("../../../../i3-src/etc/config.keycodes");
    let template_path = if from_src.exists() { from_src } else { vendored };
    let template = std::fs::read_to_string(&template_path)
        .unwrap_or_else(|e| panic!("read {}: {}", template_path.display(), e));

    let mut keymap = FakeKeymap::us();
    let mut out: Vec<u8> = Vec::new();
    finish(&mut keymap, &template, &mut out, Modifier::MOD_Mod4).expect("finish");
    let output = String::from_utf8(out).expect("utf-8");

    // C: main.c:707-712 — the header is written verbatim.
    assert!(output.starts_with(GENERATED_HEADER));

    // C: main.c:726-729 — the WARNING block of the template is dropped.
    assert!(!output.contains("# WARNING"));
    // C: main.c:742-749 — the modifier the user chose.
    assert!(output.contains("\nset $mod Mod4\n"));
    // C: main.c:753-759 — no bindcode line survives.
    assert!(
        output
            .lines()
            .all(|l| !l.trim_start().starts_with("bindcode")),
        "every bindcode line must be rewritten"
    );
    // The rewritten anchors.
    assert!(output.contains("bindsym $mod+Return exec i3-sensible-terminal\n"));
    assert!(output.contains("bindsym $mod+Shift+q kill\n"));
    assert!(output.contains("bindsym $mod+1 workspace number $ws1\n"));
    assert!(output.contains("bindsym $mod+Shift+space floating toggle\n"));
    assert!(output.contains("bindsym $mod+Shift+e exec \"i3-nagbar"));

    // C: main.c:753-756 — lines that are not bindcode are copied verbatim,
    // including the commented-out bindcode examples.
    assert!(output.contains("# bindcode $mod+40 exec \"rofi -modi drun,run -show drun\"\n"));
    assert!(output.contains("font pango:monospace 8\n"));
}

#[test]
fn finish_uses_mod1_for_the_alt_modifier() {
    let mut keymap = FakeKeymap::us();
    let mut out: Vec<u8> = Vec::new();
    finish(
        &mut keymap,
        "# WARNING\nset $mod Mod1\nbindcode $mod+36 exec x\n",
        &mut out,
        Modifier::MOD_Mod1,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("{}set $mod Mod1\nbindsym $mod+Return exec x\n", GENERATED_HEADER)
    );
}

#[test]
fn finish_keeps_the_indentation_and_skips_only_the_leading_warning_block() {
    let mut keymap = FakeKeymap::us();
    let mut out: Vec<u8> = Vec::new();
    let template = concat!(
        "# WARNING\n",
        "# WARNING: still part of the block\n",
        "# WARNING\n",
        "\n",
        "  indented line\n",
        "# WARNING not at the head any more\n",
        "    bindcode 36 mode \"default\"\n",
    );
    finish(&mut keymap, template, &mut out, Modifier::MOD_Mod4).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "{}\n  indented line\n# WARNING not at the head any more\n    bindsym Return mode \"default\"\n",
            GENERATED_HEADER
        )
    );
}

#[test]
fn welcome_step_draws_the_c_texts_at_the_c_coordinates() {
    let mut wizard = wizard_for_welcome();
    let mut renderer = RecordingRenderer::new();
    assert_eq!(handle_expose(&mut wizard, &mut renderer), 1);

    // col_x(2) = 12, row_y(2) = 20, max_width = 300 - x - 4.
    assert_eq!(
        renderer.ops,
        vec![
            Op::Clear("#000000"),
            Op::Bold(false),
            Op::Text { text: "You have not configured i3 yet.".to_string(), x: 12, y: 20, max_width: 284 },
            Op::Text { text: "Do you want me to generate a config at".to_string(), x: 12, y: 36, max_width: 284 },
            Op::Text { text: "/home/u/.config/i3/config?".to_string(), x: 12, y: 52, max_width: 284 },
            Op::Text { text: "Yes, generate the config".to_string(), x: 100, y: 84, max_width: 196 },
            Op::Text { text: "No, I will use the defaults".to_string(), x: 100, y: 116, max_width: 196 },
            Op::Text { text: "<Enter>".to_string(), x: 28, y: 84, max_width: 268 },
            Op::Text { text: "<ESC>".to_string(), x: 36, y: 116, max_width: 260 },
            Op::Flush,
        ]
    );
}

#[test]
fn generate_step_marks_the_selected_modifier_in_bold() {
    let mut wizard = wizard_for_welcome();
    wizard.current_step = Step::STEP_GENERATE;
    wizard.modifier = Modifier::MOD_Mod4;
    let mut renderer = RecordingRenderer::new();
    handle_expose(&mut wizard, &mut renderer);

    let texts: Vec<&Op> = renderer
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Text { .. }))
        .collect();
    // C: main.c:536-561 — the order of the texts.
    let expected = [
        ("Please choose either:", 12, 20),
        ("Win as default modifier", 100, 52),
        ("Alt as default modifier", 100, 68),
        ("Afterwards, press", 12, 100),
        ("to write the config", 100, 132),
        ("to abort", 100, 148),
        ("<Alt>", 36, 68),
        ("-> <Win>", 12, 52),
        ("<Enter>", 28, 132),
        ("<ESC>", 36, 148),
    ];
    assert_eq!(texts.len(), expected.len());
    for (op, (text, x, y)) in texts.iter().zip(expected.iter()) {
        match op {
            Op::Text { text: got, x: gx, y: gy, .. } => {
                assert_eq!((got.as_str(), *gx, *gy), (*text, *x, *y));
            }
            _ => unreachable!(),
        }
    }
    // The bold font is set around the selected line only.
    assert_eq!(
        renderer.ops.iter().filter(|o| **o == Op::Bold(true)).count(),
        1
    );
    assert_eq!(
        renderer.ops.iter().filter(|o| **o == Op::Bold(false)).count(),
        2
    );

    // With Alt selected, the marks swap.
    let mut wizard = wizard_for_welcome();
    wizard.current_step = Step::STEP_GENERATE;
    wizard.modifier = Modifier::MOD_Mod1;
    let mut renderer = RecordingRenderer::new();
    handle_expose(&mut wizard, &mut renderer);
    let texts: Vec<String> = renderer
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(texts.contains(&"<Win>".to_string()));
    assert!(texts.contains(&"-> <Alt>".to_string()));
}

#[test]
fn return_advances_from_welcome_then_writes_the_config() {
    let mut wizard = wizard_for_welcome();
    let mut keymap = FakeKeymap::us();
    let mut renderer = RecordingRenderer::new();

    let outcome = handle_key_press(
        &mut wizard,
        36,
        0,
        &mut keymap,
        &[],
        0,
        &mut renderer,
    );
    assert_eq!(outcome, KeyOutcome::Continue);
    assert_eq!(wizard.current_step, Step::STEP_GENERATE);
    // C: main.c:586-594 — the window title changes.
    assert!(renderer
        .ops
        .iter()
        .any(|op| *op == Op::Title("i3: generate config".to_string())));

    // The second Return finishes.
    let outcome = handle_key_press(
        &mut wizard,
        36,
        0,
        &mut keymap,
        &[],
        0,
        &mut renderer,
    );
    assert_eq!(outcome, KeyOutcome::Finish);
}

#[test]
fn escape_cancels_at_any_time() {
    let mut wizard = wizard_for_welcome();
    let mut keymap = FakeKeymap::us();
    let mut renderer = RecordingRenderer::new();
    assert_eq!(
        handle_key_press(&mut wizard, 9, 0, &mut keymap, &[], 0, &mut renderer),
        KeyOutcome::Exit
    );
    // Also in the generate step.
    wizard.current_step = Step::STEP_GENERATE;
    assert_eq!(
        handle_key_press(&mut wizard, 9, 0, &mut keymap, &[], 0, &mut renderer),
        KeyOutcome::Exit
    );
}

#[test]
fn up_and_down_swap_the_modifier() {
    let mut wizard = wizard_for_welcome();
    wizard.current_step = Step::STEP_GENERATE;
    wizard.modifier = Modifier::MOD_Mod4;
    let mut keymap = FakeKeymap::us();
    let mut renderer = RecordingRenderer::new();
    assert_eq!(
        handle_key_press(&mut wizard, 111, 0, &mut keymap, &[], 0, &mut renderer),
        KeyOutcome::Continue
    );
    assert_eq!(wizard.modifier, Modifier::MOD_Mod1);
    handle_key_press(&mut wizard, 116, 0, &mut keymap, &[], 0, &mut renderer);
    assert_eq!(wizard.modifier, Modifier::MOD_Mod4);
    // Each toggle redraws.
    assert!(renderer.ops.contains(&Op::Flush));
}

#[test]
fn pressing_a_modifier_key_selects_it() {
    // C: main.c:613-640 — the modmap contains Shift, Lock, Control, Mod1,
    // Mod2, Mod3, Mod4, Mod5 with keycodes_per_modifier entries each.
    let mut wizard = wizard_for_welcome();
    wizard.current_step = Step::STEP_GENERATE;
    wizard.modifier = Modifier::MOD_Mod4;
    let mut keymap = FakeKeymap::us();
    let mut renderer = RecordingRenderer::new();

    // 4 keycodes per modifier: index 3*4 = Mod1, index 6*4 = Mod4.
    let mut modmap = vec![0u8; 8 * 4];
    modmap[3 * 4] = 64; // Alt_L is Mod1
    modmap[6 * 4] = 133; // Super_L is Mod4

    handle_key_press(&mut wizard, 64, 0, &mut keymap, &modmap, 4, &mut renderer);
    assert_eq!(wizard.modifier, Modifier::MOD_Mod1);

    handle_key_press(&mut wizard, 133, 0, &mut keymap, &modmap, 4, &mut renderer);
    assert_eq!(wizard.modifier, Modifier::MOD_Mod4);
}

#[test]
fn clicks_on_the_modifier_rows_select_them() {
    // C: main.c:650-668 — only in the generate step, only inside the columns
    // 5..10 and only on the two modifier rows.
    let mut wizard = wizard_for_welcome();
    let mut renderer = RecordingRenderer::new();

    // Welcome step: ignored.
    handle_button_press(&mut wizard, 50, 52, &mut renderer);
    assert_eq!(wizard.modifier, Modifier::MOD_Mod4);
    assert!(renderer.ops.is_empty());

    wizard.current_step = Step::STEP_GENERATE;
    // Outside of the columns: ignored.
    handle_button_press(&mut wizard, col_x(11, 8, 4) as i16, 52, &mut renderer);
    assert!(renderer.ops.is_empty());

    // Row 4 selects Win.
    wizard.modifier = Modifier::MOD_Mod1;
    handle_button_press(&mut wizard, col_x(6, 8, 4) as i16, row_y(4, 16, 4) as i16, &mut renderer);
    assert_eq!(wizard.modifier, Modifier::MOD_Mod4);
    assert!(renderer.ops.contains(&Op::Flush));

    // Row 5 selects Alt.
    renderer.ops.clear();
    handle_button_press(&mut wizard, col_x(6, 8, 4) as i16, row_y(5, 16, 4) as i16, &mut renderer);
    assert_eq!(wizard.modifier, Modifier::MOD_Mod1);
    assert!(renderer.ops.contains(&Op::Flush));
}

#[test]
fn keysym_names_cover_the_common_cases() {
    assert_eq!(keysym_name(0x61).as_deref(), Some("a"));
    assert_eq!(keysym_name(0x31).as_deref(), Some("1"));
    assert_eq!(keysym_name(0x20).as_deref(), Some("space"));
    assert_eq!(keysym_name(0x2d).as_deref(), Some("minus"));
    assert_eq!(keysym_name(0xff0d).as_deref(), Some("Return"));
    assert_eq!(keysym_name(0xff51).as_deref(), Some("Left"));
    assert_eq!(keysym_name(0xffbe).as_deref(), Some("F1"));
    assert_eq!(keysym_name(0xffc9).as_deref(), Some("F12"));
    assert_eq!(keysym_name(0x0130), None);
}

#[test]
fn col_and_row_helpers_match_the_c_macros() {
    // C: main.c:77-80
    assert_eq!(col_x(1, 7, 4), 4);
    assert_eq!(col_x(13, 7, 4), 88);
    assert_eq!(row_y(1, 16, 4), 4);
    assert_eq!(row_y(10, 16, 4), 148);
}

#[test]
fn rewrite_binding_gives_up_on_unparsable_input() {
    // C would loop forever on an input no token matches; the port returns
    // None instead of hanging (see the comment in rewrite_binding).
    let mut parser = binding_parser::default();
    let mut keymap = FakeKeymap::us();
    assert_eq!(parser.rewrite_binding("!!!\n", &mut keymap), None);
}
