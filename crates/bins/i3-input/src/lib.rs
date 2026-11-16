//! C: i3-input/main.c (551 lines) + i3-input/keysym2ucs.c (849 lines) —
//! Utility which lets the user input commands and sends them to i3.
//!
//! The X11 parts (window creation, keyboard grab, drawing) need a real X
//! server, so the drawing and the keysym lookup are behind small traits: the
//! production implementations call libi3/xcb, the tests use recording mocks so
//! that the 1:1 draw order and geometry of C can be asserted without X.
//!
//! The pure logic (glyph buffer, %s substitution, keysym2ucs) is byte-exact
//! with C and is covered by unit tests.

#![allow(non_camel_case_types, non_snake_case)]

pub mod keysyms;
pub mod keysym2ucs;

use libi3::draw_util::color_t;
use libi3::string::{i3string_from_ucs2, i3string_from_utf8, i3String};
use libi3::xcb_char2b_t;

pub use keysym2ucs::keysym2ucs;

/// C: i3-input/main.c:31 #define MAX_WIDTH logical_px(500) — the logical value
/// (logical_px() itself depends on the X server's DPI).
const MAX_WIDTH_LOGICAL: i32 = 500;
/// C: i3-input/main.c:32 #define BORDER logical_px(2)
const BORDER_LOGICAL: i32 = 2;
/// C: i3-input/main.c:33 #define PADDING logical_px(2)
const PADDING_LOGICAL: i32 = 2;

/* C: i3-input/main.c:35-41 — Exit codes for i3-input:
 * 0 if i3-input exited successfully and the command was run
 * 1 if the user canceled input
 * 2 if i3-input fails for any other reason */
pub const EXIT_OK: i32 = 0;
pub const EXIT_CANCEL: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

/// C: i3-input/main.c:52 static xcb_char2b_t glyphs_ucs[512]
pub const MAX_GLYPHS: usize = 512;

/// C: i3-input/main.c:47-58 — the file-static state of the input window.
pub struct Input {
    /// C: xcb_char2b_t glyphs_ucs[512]
    pub glyphs_ucs: Vec<xcb_char2b_t>,
    /// C: char *glyphs_utf8[512] (NULL after a free())
    pub glyphs_utf8: Vec<Option<String>>,
    /// C: int input_position
    pub input_position: usize,
    /// C: i3String *prompt
    pub prompt: Option<i3String>,
    /// C: int prompt_offset
    pub prompt_offset: i32,
    /// C: int limit
    pub limit: i32,
    /// C: char *format — the IPC format string, %s is replaced by the input
    pub format: String,
    /// C: bool modeswitch_active
    pub modeswitch_active: bool,
    /// C: static int sockfd
    pub sockfd: i32,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            // C: static arrays of 512 entries, zero-initialised.
            glyphs_ucs: vec![xcb_char2b_t::default(); MAX_GLYPHS],
            glyphs_utf8: vec![None; MAX_GLYPHS],
            input_position: 0,
            prompt: None,
            prompt_offset: 0,
            limit: 0,
            format: String::new(),
            modeswitch_active: false,
            sockfd: -1,
        }
    }
}

impl Input {
    /// C: i3-input/main.c:91 static uint8_t *concat_strings(char **glyphs, int max)
    ///
    /// Concats the glyphs (either UCS-2 or UTF-8) to a single string, suitable
    /// for sending it to i3 (UTF-8). C decides per glyph: if the first byte is
    /// 0 it is UCS-2 and two bytes are copied, otherwise the UTF-8 string is
    /// copied up to its NUL byte.
    pub fn concat_strings(glyphs: &[Option<String>], max: usize) -> Vec<u8> {
        // C: uint8_t *output = scalloc(max + 1, 4);
        let mut output: Vec<u8> = vec![0u8; (max + 1) * 4];
        let mut walk: usize = 0;
        for c in 0..max {
            let glyph = match &glyphs[c] {
                Some(g) => g.as_bytes(),
                None => b"",
            };
            println!("at {}", *glyph.first().unwrap_or(&0) as char);
            if glyph.first().copied().unwrap_or(0) == 0 {
                // C: memcpy(walk, glyphs[c], 2); walk += 2;
                for i in 0..2 {
                    if walk + i < output.len() {
                        output[walk + i] = *glyph.get(i).unwrap_or(&0);
                    }
                }
                walk += 2;
            } else {
                // C: strcpy(walk, glyphs[c]); walk += strlen(glyphs[c]);
                let n = glyph.iter().position(|&b| b == 0).unwrap_or(glyph.len());
                let end = (walk + n).min(output.len());
                output[walk..end].copy_from_slice(&glyph[..end - walk]);
                walk = end;
            }
        }
        // C: printf("output = %s\n", (char *)output) — %s stops at the NUL.
        let nul = output.iter().position(|&b| b == 0).unwrap_or(output.len());
        println!("output = {}", String::from_utf8_lossy(&output[..nul]));
        output
    }
}

/// C: libc atoi(3) — the subset i3-input relies on for -l: leading whitespace,
/// optional sign, decimal digits, 0 when no digits follow.
pub fn atoi(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || (bytes[i] >= 0x09 && bytes[i] <= 0x0d)) {
        i += 1;
    }
    let mut sign = 1i64;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        if bytes[i] == b'-' {
            sign = -1;
        }
        i += 1;
    }
    let mut value: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value * 10 + (bytes[i] - b'0') as i64;
        i += 1;
    }
    (sign * value) as i32
}

/// C: i3-input/main.c:162 static void finish_input(void) — part of the function:
/// the %s substitution into the format string.
///
/// C counts the occurrences of "%s" (finish_input) and then walks the format
/// once, copying either the character or the whole input. The port walks the
/// bytes the same way, the MAX(input_len, ...) allocation of C is not needed
/// because the result is built in a growable buffer.
pub fn substitute_format(format: &str, command: &[u8]) -> Vec<u8> {
    let format = format.as_bytes();
    let len = format.len();
    let input_len = command.len();
    let mut full: Vec<u8> = Vec::with_capacity(len + input_len);
    let mut c = 0usize;
    while c < len {
        // C: if (format[c] != '%' || c == len - 1 || format[c + 1] != 's')
        if format[c] != b'%' || c + 1 >= len || format[c + 1] != b's' {
            full.push(format[c]);
        } else {
            full.extend_from_slice(command);
            // C: skip the following 's' of "%s"
            c += 1;
        }
        c += 1;
    }
    let _ = input_len;
    full
}

/// The number of "%s" occurrences in the format string, as finish_input() counts
/// them (C: for (c = 0; c < (len - 1); c++) if (format[c] == '%' &&
/// format[c + 1] == 's') cnt++).
pub fn count_occurrences(format: &str) -> usize {
    let format = format.as_bytes();
    let len = format.len();
    let mut cnt = 0usize;
    for c in 0..len.saturating_sub(1) {
        if format[c] == b'%' && format[c + 1] == b's' {
            cnt += 1;
        }
    }
    cnt
}

/*
 * C: i3-input/main.c:24 xcb_visualtype_t *visual_type — the drawing calls of C
 * go through libi3's draw_util/font (cairo), which are not ported yet. The
 * trait below is the seam: RealRenderer forwards to libi3, RecordingRenderer
 * (tests) records the calls so that the C order and geometry can be asserted.
 */

/// The drawing surface as used by i3-input (C: surface_t plus the font and DPI
/// helpers of libi3).
pub trait InputRenderer {
    /// C: libi3/dpi.c:85 int logical_px(const int logical)
    fn logical_px(&mut self, logical: i32) -> i32;
    /// C: libi3/draw_util.c:163 color_t draw_util_hex_to_color(const char *color)
    fn hex_to_color(&mut self, color: &str) -> color_t;
    /// C: libi3/draw_util.c:256 void draw_util_rectangle(...)
    fn rectangle(&mut self, color: color_t, x: f64, y: f64, width: f64, height: f64);
    /// C: libi3/draw_util.c:209 void draw_util_text(...)
    fn text(
        &mut self,
        text: &mut i3String,
        fg_color: color_t,
        bg_color: color_t,
        x: i32,
        y: i32,
        max_width: i32,
    );
    /// C: surface.width
    fn surface_width(&self) -> i32;
    /// C: surface.height
    fn surface_height(&self) -> i32;
    /// C: font.height
    fn font_height(&self) -> i32;
    /// C: libi3/font.c:466 int predict_text_width(i3String *text)
    fn predict_text_width(&mut self, text: &mut i3String) -> i32;
    /// C: xcb_flush(conn)
    fn flush(&mut self);
}

/// C: i3-input/main.c:114 static int handle_expose(xcb_connection_t *conn)
///
/// Draws the border, the background, the prompt and the current input.
pub fn handle_expose(input: &mut Input, renderer: &mut dyn InputRenderer) -> i32 {
    println!("expose!");

    let border_color = renderer.hex_to_color("#FF0000");
    let fg_color = renderer.hex_to_color("#FFFFFF");
    let bg_color = renderer.hex_to_color("#000000");

    let border = renderer.logical_px(BORDER_LOGICAL);
    let padding = renderer.logical_px(PADDING_LOGICAL);
    let max_width = renderer.logical_px(MAX_WIDTH_LOGICAL);
    let text_offset = border + padding;

    let width = renderer.surface_width();
    let height = renderer.surface_height();

    // C: draw border
    renderer.rectangle(border_color, 0.0, 0.0, width as f64, height as f64);

    // C: draw background
    renderer.rectangle(
        bg_color,
        border as f64,
        border as f64,
        (width - 2 * border) as f64,
        (height - 2 * border) as f64,
    );

    // C: draw the prompt …
    if let Some(prompt) = input.prompt.as_mut() {
        renderer.text(
            prompt,
            fg_color,
            bg_color,
            text_offset,
            text_offset,
            max_width - text_offset,
        );
    }

    // C: … and the text
    if input.input_position > 0 {
        let mut drawn = i3string_from_ucs2(&input.glyphs_ucs, input.input_position);
        renderer.text(
            &mut drawn,
            fg_color,
            bg_color,
            text_offset + input.prompt_offset,
            text_offset,
            max_width - text_offset,
        );
        libi3::string::i3string_free(Some(drawn));
    }

    renderer.flush();

    1
}

/// C: i3-input/main.c:150 static int handle_key_release(void *ignored,
/// xcb_connection_t *conn, xcb_key_release_event_t *event)
///
/// Deactivates the Mode_switch bit upon release of the Mode_switch key.
pub fn handle_key_release(
    input: &mut Input,
    keycode: u8,
    state: u16,
    lookup: &mut dyn KeysymLookup,
) -> i32 {
    println!("releasing {}, state raw = {}", keycode, state);

    // C: xcb_key_press_lookup_keysym(symbols, event, event->state) — the C
    // code passes the state mask where the column is expected, kept 1:1.
    let sym = lookup.key_press_lookup_keysym(keycode, state as i32);
    if sym == keysyms::XK_Mode_switch {
        println!("Mode switch disabled");
        input.modeswitch_active = false;
    }

    1
}

/// C: xcb_key_symbols_t + xcb_key_press_lookup_keysym() from
/// xcb-util-keysyms (the C build links it through xcb_keysyms).
pub trait KeysymLookup {
    /// C: xcb_key_press_lookup_keysym(symbols, event, col)
    fn key_press_lookup_keysym(&mut self, keycode: u8, col: i32) -> u32;
}

/// What handle_key_press() wants the event loop to do (C exits directly at
/// these points, the port returns the exit code instead).
#[derive(Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Keep running.
    Continue,
    /// C: finish_input() + exit(ret == 0 ? EXIT_OK : EXIT_ERROR)
    Finish(i32),
    /// C: exit(EXIT_CANCEL)
    Cancel,
}

/// C: i3-input/main.c:216 static int handle_key_press(void *ignored,
/// xcb_connection_t *conn, xcb_key_press_event_t *event)
///
/// Converts the keycode to a keysym, the keysym to UCS-2 and stores the glyph.
/// Also handles backspace, return and escape.
pub fn handle_key_press(
    input: &mut Input,
    keycode: u8,
    state: u16,
    lookup: &mut dyn KeysymLookup,
    renderer: &mut dyn InputRenderer,
) -> KeyOutcome {
    println!("Keypress {}, state raw = {}", keycode, state);

    /* C: See the documentation of xcb_key_symbols_get_keysym for this one.
     * Basically: We get either col 0 or col 1, depending on whether shift is
     * pressed. */
    let mut col = (state & keysyms::XCB_MOD_MASK_SHIFT) as i32;

    /* C: If modeswitch is currently active, we need to look in group 2 or 3. */
    if input.modeswitch_active {
        col += 2;
    }

    let sym = lookup.key_press_lookup_keysym(keycode, col);
    if sym == keysyms::XK_Mode_switch {
        println!("Mode switch enabled");
        input.modeswitch_active = true;
        return KeyOutcome::Continue;
    }

    if sym == keysyms::XK_Return {
        return KeyOutcome::Finish(finish_input(input));
    }

    if sym == keysyms::XK_BackSpace {
        if input.input_position == 0 {
            return KeyOutcome::Continue;
        }

        input.input_position -= 1;
        // C: free(glyphs_utf8[input_position]);
        input.glyphs_utf8[input.input_position] = None;

        handle_expose(input, renderer);
        return KeyOutcome::Continue;
    }
    if sym == keysyms::XK_Escape {
        return KeyOutcome::Cancel;
    }

    /* C: TODO: handle all of these? — the C code prints the classification,
     * then ignores modifier and cursor keys. */
    println!("is_keypad_key = {}", keysyms::xcb_is_keypad_key(sym));
    println!("is_private_keypad_key = {}", keysyms::xcb_is_private_keypad_key(sym));
    println!("xcb_is_cursor_key = {}", keysyms::xcb_is_cursor_key(sym));
    println!("xcb_is_pf_key = {}", keysyms::xcb_is_pf_key(sym));
    println!("xcb_is_function_key = {}", keysyms::xcb_is_function_key(sym));
    println!("xcb_is_misc_function_key = {}", keysyms::xcb_is_misc_function_key(sym));
    println!("xcb_is_modifier_key = {}", keysyms::xcb_is_modifier_key(sym));

    if keysyms::xcb_is_modifier_key(sym) || keysyms::xcb_is_cursor_key(sym) {
        return KeyOutcome::Continue;
    }

    println!("sym = {} ({})", (sym & 0xff) as u8 as char, sym);

    /* C: convert the keysym to UCS */
    let ucs = keysym2ucs(sym);
    // C: uint16_t ucs = keysym2ucs(sym); if ((int16_t)ucs == -1)
    let ucs16 = ucs as u16;
    if (ucs16 as i16) == -1 {
        eprintln!("Keysym could not be converted to UCS, skipping");
        return KeyOutcome::Continue;
    }

    // C: inp.byte1 = (ucs & 0xff00) >> 2; inp.byte2 = (ucs & 0x00ff) >> 0;
    // (The shift by 2 instead of 8 is a bug in C; it is kept 1:1.)
    let inp = xcb_char2b_t {
        byte1: ((ucs16 & 0xff00) >> 2) as u8,
        byte2: (ucs16 & 0x00ff) as u8,
    };

    println!("inp.byte1 = {:02x}, inp.byte2 = {:02x}", inp.byte1, inp.byte2);
    /* C: convert it to UTF-8 */
    let out = libi3::ucs2_conversion::convert_ucs2_to_utf8(&[inp], 1);
    println!("converted to {}", out.as_deref().unwrap_or(""));

    input.glyphs_ucs[input.input_position] = inp;
    input.glyphs_utf8[input.input_position] = out;
    input.input_position += 1;

    if input.input_position as i32 == input.limit {
        return KeyOutcome::Finish(finish_input(input));
    }

    handle_expose(input, renderer);
    KeyOutcome::Continue
}

/// C: i3-input/main.c:162 static void finish_input(void)
///
/// Concats the input, substitutes it into the format string and sends the
/// result to i3. C exits here; the port returns the exit code (EXIT_OK when
/// ipc_send_message() succeeded, EXIT_ERROR otherwise) so main() can exit the
/// same way.
pub fn finish_input(input: &mut Input) -> i32 {
    let mut command = Input::concat_strings(&input.glyphs_utf8, input.input_position);
    // C: const size_t input_len = strlen(command) — the scalloc()ated buffer is
    // NUL padded, so only the bytes before the first NUL are the command.
    let nul = command.iter().position(|&b| b == 0).unwrap_or(command.len());
    command.truncate(nul);

    /* C: count the occurrences of %s in the string */
    let cnt = count_occurrences(&input.format);
    println!("occurrences = {}", cnt);

    let full = substitute_format(&input.format, &command);

    /* C: prefix the command if a prefix was specified oncommandline */
    println!("command = {}", String::from_utf8_lossy(&full));

    // C: int ret = ipc_send_message(sockfd, strlen(full), 0, (uint8_t *)full);
    let ret = libi3::ipc_send_message(input.sockfd, full.len() as u32, 0, &full);

    if ret == 0 {
        EXIT_OK
    } else {
        EXIT_ERROR
    }
}

/// C: i3-input/main.c:385 int main(int argc, char *argv[]) — the option parsing
/// part, split out so it can be tested without X.
pub struct Options {
    pub socket_path: Option<String>,
    pub pattern: Option<String>,
    pub format: String,
    pub prompt: Option<String>,
    pub limit: i32,
}

/// C: i3-input/main.c:390-399 static struct option long_options[]
fn long_options() -> Vec<libi3::getopt::option> {
    use libi3::getopt::{no_argument, required_argument, option};
    vec![
        option { name: "socket", has_arg: required_argument, val: b's' },
        option { name: "version", has_arg: no_argument, val: b'v' },
        option { name: "limit", has_arg: required_argument, val: b'l' },
        option { name: "prompt", has_arg: required_argument, val: b'P' },
        option { name: "prefix", has_arg: required_argument, val: b'p' },
        option { name: "format", has_arg: required_argument, val: b'F' },
        option { name: "font", has_arg: required_argument, val: b'f' },
        option { name: "help", has_arg: no_argument, val: b'h' },
    ]
}

/// The result of the getopt loop (C: the -v/-h early returns).
pub enum ParseOutcome {
    Version,
    Help,
    Run(Options),
}

/// C: i3-input/main.c:403-444 — the getopt_long loop.
pub fn parse_args(argv: Vec<std::ffi::OsString>) -> ParseOutcome {
    use libi3::getopt::{getopt_long, Getopt};

    let mut go = Getopt::new(argv);
    let opts = long_options();
    // C: char *options_string = "s:p:P:f:l:F:vh";
    let options_string = "s:p:P:f:l:F:vh";

    let mut socket_path: Option<String> = None;
    let mut pattern: Option<String> = None;
    let mut format: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut limit: i32 = 0;

    loop {
        let o = getopt_long(&mut go, options_string, &opts);
        if o == -1 {
            break;
        }
        let optarg = || go.optarg.as_ref().map(|a| a.to_string_lossy().into_owned());
        if o == b's' as i32 {
            socket_path = optarg();
        } else if o == b'v' as i32 {
            return ParseOutcome::Version;
        } else if o == b'p' as i32 {
            /* C: This option is deprecated, but will still work in i3 v4.1, 4.2
             * and 4.3 */
            eprintln!("i3-input: WARNING: the -p option is DEPRECATED in favor of the -F (format) option");
            format = optarg().map(|p| format!("{}%s", p));
        } else if o == b'l' as i32 {
            limit = atoi(&optarg().unwrap_or_default());
        } else if o == b'P' as i32 {
            prompt = optarg();
        } else if o == b'f' as i32 {
            pattern = optarg();
        } else if o == b'F' as i32 {
            format = optarg();
        } else if o == b'h' as i32 {
            return ParseOutcome::Help;
        }
    }

    // C: if (!format) format = "%s";
    let format = format.unwrap_or_else(|| "%s".to_string());

    ParseOutcome::Run(Options {
        socket_path,
        pattern,
        format,
        prompt,
        limit,
    })
}

/// C: i3-input/main.c:410 printf("i3-input " I3_VERSION "\n")
pub fn print_version() {
    print!("i3-input {}\n", libi3::i3_version());
}

/// C: i3-input/main.c:433-440
pub fn print_help() {
    print_version();
    print!("i3-input [-s <socket>] [-F <format>] [-l <limit>] [-P <prompt>] [-f <font>] [-v]\n");
    print!("\n");
    print!("Example:\n");
    print!("    i3-input -F 'workspace \"%s\"' -P 'Switch to workspace: '\n");
}

/// C: i3-input/main.c:385 int main(int argc, char *argv[])
pub fn run_with_args(argv: Vec<std::ffi::OsString>) -> i32 {
    match parse_args(argv) {
        ParseOutcome::Version => {
            print_version();
            return EXIT_OK;
        }
        ParseOutcome::Help => {
            print_help();
            return EXIT_OK;
        }
        ParseOutcome::Run(opts) => run_input(opts),
    }
}

/*
 * Everything below needs a real X server and the not-yet-ported cairo/pango
 * layer of libi3 (draw_util.c, font.c, dpi.c). It is written 1:1 after C but
 * is not covered by the unit tests; see the TODO list in the crate docs.
 */

/// C: xcb_key_symbols_t — the keyboard mapping, enough of xcb-util-keysyms to
/// answer xcb_key_press_lookup_keysym().
pub struct KeySymbols {
    pub min_keycode: u8,
    pub max_keycode: u8,
    pub syms_per_code: usize,
    pub syms: Vec<u32>,
}

impl KeysymLookup for KeySymbols {
    /// C: xcb-util-keysyms xcb_key_press_lookup_keysym()
    fn key_press_lookup_keysym(&mut self, keycode: u8, col: i32) -> u32 {
        // C: xcb_key_symbols_get_keysym(): keysym = syms[(keycode -
        // min_keycode) * syms_per_code + col], XK_NoSymbol outside the range.
        if keycode < self.min_keycode || keycode > self.max_keycode {
            return keysyms::XK_NoSymbol;
        }
        let mut col = col as usize;
        if col >= self.syms_per_code {
            col = 0;
        }
        let idx = (keycode - self.min_keycode) as usize * self.syms_per_code + col;
        *self.syms.get(idx).unwrap_or(&keysyms::XK_NoSymbol)
    }
}

/// The production renderer: forwards to libi3 (cairo/pango) exactly like the C
/// calls do. libi3's draw_util/font/dpi are still TODO in this port, so a run
/// with a real X server stops at those stubs.
pub struct Libi3Renderer<'a> {
    pub conn: &'a x11rb::rust_connection::RustConnection,
    pub font: libi3::font::i3Font,
    pub width: i32,
    pub height: i32,
    /// C: the surface_t the drawing calls write to (surface_t::none() means
    /// "not initialized yet", which the draw_util_* functions skip).
    pub surface: libi3::draw_util::surface_t,
}

impl<'a> InputRenderer for Libi3Renderer<'a> {
    fn logical_px(&mut self, logical: i32) -> i32 {
        libi3::dpi::logical_px(logical)
    }
    fn hex_to_color(&mut self, color: &str) -> color_t {
        libi3::draw_util::draw_util_hex_to_color(color)
    }
    fn rectangle(&mut self, color: color_t, x: f64, y: f64, width: f64, height: f64) {
        libi3::draw_util::draw_util_rectangle(self.conn, &mut self.surface, color, x, y, width, height);
    }
    fn text(
        &mut self,
        text: &mut i3String,
        fg_color: color_t,
        bg_color: color_t,
        x: i32,
        y: i32,
        max_width: i32,
    ) {
        libi3::draw_util::draw_util_text(
            self.conn,
            text,
            &mut self.surface,
            fg_color,
            bg_color,
            x,
            y,
            max_width,
        );
    }
    fn surface_width(&self) -> i32 {
        self.width
    }
    fn surface_height(&self) -> i32 {
        self.height
    }
    fn font_height(&self) -> i32 {
        self.font.height
    }
    fn predict_text_width(&mut self, text: &mut i3String) -> i32 {
        libi3::font::predict_text_width(Some(self.conn), text)
    }
    fn flush(&mut self) {
        use x11rb::connection::Connection;
        let _ = self.conn.flush();
    }
}

/// C: i3-input/main.c:302 static xcb_rectangle_t get_window_position(void)
fn get_window_position(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
    font_height: i32,
) -> (i16, i16, i32, i32) {
    let border = libi3::dpi::logical_px(BORDER_LOGICAL);
    let padding = libi3::dpi::logical_px(PADDING_LOGICAL);
    let max_width = libi3::dpi::logical_px(MAX_WIDTH_LOGICAL);
    // C: xcb_rectangle_t result = {logical_px(50), logical_px(50), MAX_WIDTH,
    //     font.height + 2 * BORDER + 2 * PADDING};
    let result = (
        libi3::dpi::logical_px(50) as i16,
        libi3::dpi::logical_px(50) as i16,
        max_width,
        font_height + 2 * border + 2 * padding,
    );
    // C: then the EWMH support window / input focus / translate coordinates
    // part adjusts result.x/result.y; it needs the EWMH properties and is left
    // to the X11 port (see the TODO list).
    let _ = (conn, root);
    result
}

/// C: i3-input/main.c:385-551 — the X11 half of i3-input.
fn run_input(opts: Options) -> i32 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, EventMask, WindowClass};

    println!("using format \"{}\"", opts.format);

    // C: conn = xcb_connect(NULL, &screen);
    let (conn, screen_num) = match x11rb::rust_connection::RustConnection::connect(None) {
        Ok(c) => c,
        Err(_) => libi3::errx(1, "Cannot open display"),
    };
    let screen = match conn.setup().roots.get(screen_num) {
        Some(s) => s,
        None => libi3::errx(1, "Cannot open display"),
    };
    let root = screen.root;

    // C: sockfd = ipc_connect(socket_path);
    let sockfd = libi3::ipc_connect(opts.socket_path.as_deref());

    // C: symbols = xcb_key_symbols_alloc(conn);
    let min_keycode = conn.setup().min_keycode;
    let max_keycode = conn.setup().max_keycode;
    let count = (max_keycode - min_keycode + 1) as u8;
    let mut symbols = match conn.get_keyboard_mapping(min_keycode, count) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => KeySymbols {
                min_keycode,
                max_keycode,
                syms_per_code: reply.keysyms.len() / (count.max(1)) as usize,
                syms: reply.keysyms.iter().map(|k| *k as u32).collect(),
            },
            Err(_) => KeySymbols {
                min_keycode,
                max_keycode,
                syms_per_code: 0,
                syms: Vec::new(),
            },
        },
        Err(_) => KeySymbols {
            min_keycode,
            max_keycode,
            syms_per_code: 0,
            syms: Vec::new(),
        },
    };

    // C: init_dpi(); font = load_font(pattern ? pattern : "pango:monospace 8", true);
    libi3::dpi::init_dpi(Some(&conn));
    let font = libi3::font::load_font(
        Some(&conn),
        opts.pattern.as_deref().unwrap_or("pango:monospace 8"),
        true,
    );
    libi3::font::set_font(&font);

    let mut input = Input {
        format: opts.format.clone(),
        limit: opts.limit,
        sockfd,
        ..Default::default()
    };

    if let Some(prompt) = &opts.prompt {
        input.prompt = Some(i3string_from_utf8(prompt));
    }

    if let Some(prompt) = input.prompt.as_mut() {
        input.prompt_offset = libi3::font::predict_text_width(Some(&conn), prompt);
    }

    let win_pos = get_window_position(&conn, root, font.height);

    // C: win = xcb_generate_id(conn);
    let win = match conn.generate_id() {
        Ok(w) => w,
        Err(_) => libi3::errx(1, "Cannot generate window id"),
    };

    // C: xcb_create_window(..., XCB_CW_BACK_PIXEL | XCB_CW_OVERRIDE_REDIRECT |
    //     XCB_CW_EVENT_MASK, { 0, 1, XCB_EVENT_MASK_EXPOSURE })
    let _ = conn.create_window(
        x11rb::COPY_FROM_PARENT as u8,
        win,
        root,
        win_pos.0,
        win_pos.1,
        win_pos.2 as u16,
        win_pos.3 as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new()
            .background_pixel(0)
            .override_redirect(1)
            .event_mask(EventMask::EXPOSURE),
    );

    // C: xcb_map_window(conn, win);
    let _ = conn.map_window(win);

    // C: draw_util_surface_init(conn, &surface, win, get_visualtype(root_screen),
    //     win_pos.width, win_pos.height);
    // (the surface itself lives in the renderer below)
    let _ = conn.flush();

    // C: Try (repeatedly, if necessary) to grab the keyboard.
    use x11rb::protocol::xproto::{GrabMode, GrabStatus};
    let mut status = GrabStatus::ALREADY_GRABBED;
    let mut count_retries = 0;
    loop {
        let reply = conn
            .grab_keyboard(
                false,
                win,
                x11rb::CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok());
        match reply {
            Some(reply) => {
                status = reply.status;
                if status == GrabStatus::SUCCESS || count_retries >= 500 {
                    break;
                }
            }
            None => {
                if count_retries >= 500 {
                    break;
                }
            }
        }
        count_retries += 1;
        std::thread::sleep(std::time::Duration::from_micros(1000));
    }

    if status != GrabStatus::SUCCESS {
        eprintln!("Could not grab keyboard, status = {}", u8::from(status));
        return EXIT_ERROR;
    }

    let _ = conn.flush();

    let mut renderer = Libi3Renderer {
        conn: &conn,
        font,
        width: win_pos.2,
        height: win_pos.3,
        // C: draw_util_surface_init() for the input window.
        surface: libi3::draw_util::surface_t::none(),
    };

    // C: while ((event = xcb_wait_for_event(conn)) != NULL)
    loop {
        let event = match conn.wait_for_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        use x11rb::protocol::Event;
        match event {
            Event::KeyPress(e) => {
                let outcome = handle_key_press(
                    &mut input,
                    e.detail,
                    u16::from(e.state),
                    &mut symbols,
                    &mut renderer,
                );
                match outcome {
                    KeyOutcome::Continue => {}
                    KeyOutcome::Finish(code) => return code,
                    KeyOutcome::Cancel => return EXIT_CANCEL,
                }
            }
            Event::KeyRelease(e) => {
                handle_key_release(&mut input, e.detail, u16::from(e.state), &mut symbols);
            }
            Event::Expose(e) => {
                // C: if (((xcb_expose_event_t *)event)->count == 0)
                if e.count == 0 {
                    handle_expose(&mut input, &mut renderer);
                }
            }
            Event::Error(e) => {
                eprintln!("X11 Error received! sequence {:x}", e.sequence);
            }
            _ => {}
        }
    }

    let _ = conn.flush();
    EXIT_OK
}

/// C: i3-input/main.c:385 int main(int argc, char *argv[])
pub fn run() -> i32 {
    run_with_args(std::env::args_os().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recording mock the tests use instead of cairo: every drawing call is
    /// appended to "ops" in the order libi3 would have received it.
    #[derive(Debug, PartialEq)]
    enum Op {
        Rectangle { color: &'static str, x: f64, y: f64, w: f64, h: f64 },
        Text { text: String, x: i32, y: i32, max_width: i32 },
        Flush,
    }

    struct RecordingRenderer {
        pub ops: Vec<Op>,
        pub dpi_scale: i32,
        pub width: i32,
        pub height: i32,
        pub font_height: i32,
        pub char_width: i32,
    }

    impl RecordingRenderer {
        fn new() -> RecordingRenderer {
            RecordingRenderer {
                ops: Vec::new(),
                dpi_scale: 1,
                width: 500,
                height: 20,
                font_height: 16,
                char_width: 8,
            }
        }
    }

    fn color_of(name: &str) -> color_t {
        let mut c = color_t::default();
        c.red = match name {
            "#FF0000" => 1.0,
            "#FFFFFF" => 1.0,
            _ => 0.0,
        };
        c.colorpixel = match name {
            "#FF0000" => 0xff0000,
            "#FFFFFF" => 0xffffff,
            _ => 0x000000,
        };
        c
    }

    fn color_name(color: color_t) -> &'static str {
        match color.colorpixel {
            0xff0000 => "#FF0000",
            0xffffff => "#FFFFFF",
            _ => "#000000",
        }
    }

    impl InputRenderer for RecordingRenderer {
        fn logical_px(&mut self, logical: i32) -> i32 {
            logical * self.dpi_scale
        }
        fn hex_to_color(&mut self, color: &str) -> color_t {
            color_of(color)
        }
        fn rectangle(&mut self, color: color_t, x: f64, y: f64, width: f64, height: f64) {
            self.ops.push(Op::Rectangle {
                color: color_name(color),
                x,
                y,
                w: width,
                h: height,
            });
        }
        fn text(
            &mut self,
            text: &mut i3String,
            fg_color: color_t,
            bg_color: color_t,
            x: i32,
            y: i32,
            max_width: i32,
        ) {
            let _ = (fg_color, bg_color);
            let s = libi3::string::i3string_as_utf8(text).unwrap_or("").to_string();
            self.ops.push(Op::Text {
                text: s,
                x,
                y,
                max_width,
            });
        }
        fn surface_width(&self) -> i32 {
            self.width
        }
        fn surface_height(&self) -> i32 {
            self.height
        }
        fn font_height(&self) -> i32 {
            self.font_height
        }
        fn predict_text_width(&mut self, text: &mut i3String) -> i32 {
            libi3::string::i3string_get_num_glyphs(text) as i32 * self.char_width
        }
        fn flush(&mut self) {
            self.ops.push(Op::Flush);
        }
    }

    /// C: xcb_key_symbols — a fixed keycode/column table for the tests.
    struct FakeKeysyms {
        pub map: Vec<(u8, i32, u32)>,
    }

    impl KeysymLookup for FakeKeysyms {
        fn key_press_lookup_keysym(&mut self, keycode: u8, col: i32) -> u32 {
            for (k, c, sym) in &self.map {
                if *k == keycode && *c == col {
                    return *sym;
                }
            }
            keysyms::XK_NoSymbol
        }
    }

    fn test_input() -> Input {
        Input {
            format: "%s".to_string(),
            prompt: Some(i3string_from_utf8("pw: ")),
            ..Default::default()
        }
    }

    #[test]
    fn keysym2ucs_representative_points() {
        // Latin-1 is a 1:1 mapping.
        assert_eq!(keysym2ucs(0x0041), 0x41);
        assert_eq!(keysym2ucs(0x00e4), 0xe4);
        // Below 0x20 and the gap 0x7f..0x9f are not 1:1 mapped.
        assert_eq!(keysym2ucs(0x0009), -1);
        assert_eq!(keysym2ucs(0x0080), -1);
        // Encoded 24-bit UCS: 0x1000000 | codepoint.
        assert_eq!(keysym2ucs(0x01000041), 0x41);
        assert_eq!(keysym2ucs(0x010020ac), 0x20ac);
        // Table lookups: Aogonek, EuroSign, Hangul_J_Hieuh.
        assert_eq!(keysym2ucs(0x01a1), 0x0104);
        assert_eq!(keysym2ucs(0x20ac), 0x20ac);
        assert_eq!(keysym2ucs(0x0eee), 0x11c2);
        // Keysyms without a Unicode value.
        assert_eq!(keysym2ucs(0xff0d), -1); // XK_Return
        assert_eq!(keysym2ucs(0xffe1), -1); // XK_Shift_L
    }

    #[test]
    fn keysymtab_is_sorted_and_has_the_c_size() {
        // C: sizeof(keysymtab) / sizeof(struct codepair) — 739 entries.
        assert_eq!(keysym2ucs::keysymtab.len(), 739);
        for w in keysym2ucs::keysymtab.windows(2) {
            assert!(
                w[0].0 < w[1].0,
                "keysymtab must stay sorted: {:04x} < {:04x}",
                w[0].0,
                w[1].0
            );
        }
    }

    #[test]
    fn substitute_format_replaces_every_placeholder() {
        assert_eq!(substitute_format("%s", b"nop"), b"nop".to_vec());
        assert_eq!(
            substitute_format("workspace \"%s\"", b"2: mail"),
            b"workspace \"2: mail\"".to_vec()
        );
        assert_eq!(
            substitute_format("exec %s; %s", b"x"),
            b"exec x; x".to_vec()
        );
        // No placeholder at all.
        assert_eq!(substitute_format("reload", b"x"), b"reload".to_vec());
        // A trailing '%' is copied verbatim (C: c == len - 1).
        assert_eq!(substitute_format("100%", b"x"), b"100%".to_vec());
        // "%%s": the first '%' is copied, then "%s" is not a placeholder at
        // the second position because the previous character is not consumed.
        assert_eq!(substitute_format("a%%s", b"x"), b"a%x".to_vec());
        // Empty input.
        assert_eq!(substitute_format("%s", b""), b"".to_vec());
    }

    #[test]
    fn occurrences_are_counted_like_c() {
        assert_eq!(count_occurrences("%s"), 1);
        assert_eq!(count_occurrences("%s %s"), 2);
        assert_eq!(count_occurrences("plain"), 0);
        // C stops at len - 1, so a trailing "%s" is still counted but a single
        // '%' is not.
        assert_eq!(count_occurrences("%"), 0);
        assert_eq!(count_occurrences("100%"), 0);
    }

    #[test]
    fn concat_strings_joins_utf8_glyphs() {
        let glyphs = vec![
            Some("a".to_string()),
            Some("\u{e4}".to_string()),
            None,
        ];
        let out = Input::concat_strings(&glyphs, 2);
        assert_eq!(&out[..3], "aä".as_bytes());
    }

    #[test]
    fn concat_strings_treats_a_leading_nul_as_ucs2() {
        // C: if (glyphs[c][0] == '\0') { memcpy(walk, glyphs[c], 2); }
        let glyphs = vec![Some("\u{0}\u{41}".to_string())];
        let out = Input::concat_strings(&glyphs, 1);
        assert_eq!(&out[..2], &[0u8, 0x41]);
    }

    #[test]
    fn atoi_matches_libc() {
        assert_eq!(atoi("12"), 12);
        assert_eq!(atoi("  -5"), -5);
        assert_eq!(atoi("+7x"), 7);
        assert_eq!(atoi("abc"), 0);
        assert_eq!(atoi(""), 0);
    }

    #[test]
    fn expose_draws_border_background_prompt_and_input() {
        let mut input = test_input();
        input.input_position = 2;
        input.glyphs_ucs[0] = xcb_char2b_t { byte1: 0, byte2: b'a' };
        input.glyphs_ucs[1] = xcb_char2b_t { byte1: 0, byte2: b'b' };
        input.prompt_offset = 32; // predict_text_width("pw: ") with 8px per glyph
        let mut renderer = RecordingRenderer::new();

        assert_eq!(handle_expose(&mut input, &mut renderer), 1);

        // C: i3-input/main.c:114-144 — border, background, prompt, input, flush.
        assert_eq!(
            renderer.ops,
            vec![
                Op::Rectangle { color: "#FF0000", x: 0.0, y: 0.0, w: 500.0, h: 20.0 },
                Op::Rectangle { color: "#000000", x: 2.0, y: 2.0, w: 496.0, h: 16.0 },
                Op::Text { text: "pw: ".to_string(), x: 4, y: 4, max_width: 496 },
                Op::Text { text: "ab".to_string(), x: 36, y: 4, max_width: 496 },
                Op::Flush,
            ]
        );
    }

    #[test]
    fn expose_without_input_draws_only_the_prompt() {
        let mut input = test_input();
        let mut renderer = RecordingRenderer::new();
        handle_expose(&mut input, &mut renderer);
        assert_eq!(
            renderer.ops,
            vec![
                Op::Rectangle { color: "#FF0000", x: 0.0, y: 0.0, w: 500.0, h: 20.0 },
                Op::Rectangle { color: "#000000", x: 2.0, y: 2.0, w: 496.0, h: 16.0 },
                Op::Text { text: "pw: ".to_string(), x: 4, y: 4, max_width: 496 },
                Op::Flush,
            ]
        );
    }

    #[test]
    fn mode_switch_toggles_and_is_not_a_glyph() {
        let mut input = test_input();
        let mut lookup = FakeKeysyms {
            map: vec![(50, 0, keysyms::XK_Mode_switch)],
        };
        let mut renderer = RecordingRenderer::new();
        assert_eq!(
            handle_key_press(&mut input, 50, 0, &mut lookup, &mut renderer),
            KeyOutcome::Continue
        );
        assert!(input.modeswitch_active);
        assert_eq!(input.input_position, 0);
        assert!(renderer.ops.is_empty(), "no redraw for a modifier");

        // The release deactivates it again.
        assert_eq!(
            handle_key_release(&mut input, 50, 0, &mut lookup),
            1
        );
        assert!(!input.modeswitch_active);
    }

    #[test]
    fn shift_selects_column_one() {
        let mut input = test_input();
        // keycode 38 has 'a' in column 0 and 'A' in column 1.
        let mut lookup = FakeKeysyms {
            map: vec![(38, 0, 0x61), (38, 1, 0x41)],
        };
        let mut renderer = RecordingRenderer::new();
        handle_key_press(&mut input, 38, 0, &mut lookup, &mut renderer);
        assert_eq!(input.input_position, 1);
        assert_eq!(input.glyphs_utf8[0], Some("a".to_string()));

        handle_key_press(
            &mut input,
            38,
            keysyms::XCB_MOD_MASK_SHIFT,
            &mut lookup,
            &mut renderer,
        );
        assert_eq!(input.input_position, 2);
        assert_eq!(input.glyphs_utf8[1], Some("A".to_string()));
    }

    #[test]
    fn mode_switch_uses_column_two() {
        let mut input = test_input();
        input.modeswitch_active = true;
        let mut lookup = FakeKeysyms {
            map: vec![(38, 0, 0x61), (38, 2, 0xe4)],
        };
        let mut renderer = RecordingRenderer::new();
        handle_key_press(&mut input, 38, 0, &mut lookup, &mut renderer);
        assert_eq!(input.glyphs_utf8[0], Some("\u{e4}".to_string()));
    }

    #[test]
    fn backspace_removes_the_last_glyph() {
        let mut input = test_input();
        input.glyphs_utf8[0] = Some("a".to_string());
        input.input_position = 1;
        let mut lookup = FakeKeysyms {
            map: vec![(22, 0, keysyms::XK_BackSpace)],
        };
        let mut renderer = RecordingRenderer::new();
        assert_eq!(
            handle_key_press(&mut input, 22, 0, &mut lookup, &mut renderer),
            KeyOutcome::Continue
        );
        assert_eq!(input.input_position, 0);
        assert_eq!(input.glyphs_utf8[0], None);
        // A redraw happened.
        assert!(matches!(renderer.ops.last(), Some(Op::Flush)));

        // On an empty input, backspace is a no-op without a redraw.
        let mut renderer = RecordingRenderer::new();
        assert_eq!(
            handle_key_press(&mut input, 22, 0, &mut lookup, &mut renderer),
            KeyOutcome::Continue
        );
        assert!(renderer.ops.is_empty());
    }

    #[test]
    fn escape_cancels() {
        let mut input = test_input();
        let mut lookup = FakeKeysyms {
            map: vec![(9, 0, keysyms::XK_Escape)],
        };
        let mut renderer = RecordingRenderer::new();
        assert_eq!(
            handle_key_press(&mut input, 9, 0, &mut lookup, &mut renderer),
            KeyOutcome::Cancel
        );
    }

    #[test]
    fn modifier_keys_are_ignored() {
        let mut input = test_input();
        let mut lookup = FakeKeysyms {
            map: vec![(50, 0, 0xffe1)], // XK_Shift_L
        };
        let mut renderer = RecordingRenderer::new();
        assert_eq!(
            handle_key_press(&mut input, 50, 0, &mut lookup, &mut renderer),
            KeyOutcome::Continue
        );
        assert_eq!(input.input_position, 0);
    }

    #[test]
    fn unconvertible_keysym_is_skipped() {
        let mut input = test_input();
        let mut lookup = FakeKeysyms {
            map: vec![(64, 0, 0xffbe)], // XK_F1: no UCS mapping
        };
        let mut renderer = RecordingRenderer::new();
        assert_eq!(
            handle_key_press(&mut input, 64, 0, &mut lookup, &mut renderer),
            KeyOutcome::Continue
        );
        assert_eq!(input.input_position, 0);
    }

    #[test]
    fn limit_finishes_the_input() {
        // Uses the IPC path: a socket pair stands in for i3.
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut input = test_input();
        input.sockfd = std::os::unix::io::AsRawFd::as_raw_fd(&a);
        input.limit = 1;
        let mut lookup = FakeKeysyms {
            map: vec![(38, 0, 0x61)],
        };
        let mut renderer = RecordingRenderer::new();
        let outcome = handle_key_press(&mut input, 38, 0, &mut lookup, &mut renderer);
        assert_eq!(outcome, KeyOutcome::Finish(EXIT_OK));

        // The IPC message is "a" as RUN_COMMAND (type 0).
        use std::io::Read;
        let mut b = b;
        let mut header = [0u8; 14];
        b.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..6], b"i3-ipc");
        assert_eq!(u32::from_ne_bytes(header[6..10].try_into().unwrap()), 1);
        assert_eq!(u32::from_ne_bytes(header[10..14].try_into().unwrap()), 0);
        let mut payload = [0u8; 1];
        b.read_exact(&mut payload).unwrap();
        assert_eq!(&payload, b"a");
    }

    #[test]
    fn finish_input_sends_the_substituted_command() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut input = test_input();
        input.sockfd = std::os::unix::io::AsRawFd::as_raw_fd(&a);
        input.format = "workspace \"%s\"".to_string();
        input.glyphs_utf8[0] = Some("2".to_string());
        input.input_position = 1;

        assert_eq!(finish_input(&mut input), EXIT_OK);

        use std::io::Read;
        let mut b = b;
        let mut header = [0u8; 14];
        b.read_exact(&mut header).unwrap();
        let len = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
        assert_eq!(len, "workspace \"2\"".len());
        let mut payload = vec![0u8; len];
        b.read_exact(&mut payload).unwrap();
        assert_eq!(payload, b"workspace \"2\"");
    }

    #[test]
    fn parse_args_defaults_and_flags() {
        use std::ffi::OsString;
        let argv: Vec<OsString> = ["i3-input"].iter().map(OsString::from).collect();
        match parse_args(argv) {
            ParseOutcome::Run(o) => {
                assert_eq!(o.format, "%s");
                assert_eq!(o.limit, 0);
                assert_eq!(o.prompt, None);
                assert_eq!(o.pattern, None);
                assert_eq!(o.socket_path, None);
            }
            _ => panic!("expected Run"),
        }

        let argv: Vec<OsString> = [
            "i3-input",
            "-F",
            "workspace \"%s\"",
            "-l",
            "5",
            "-P",
            "Switch: ",
            "-f",
            "pango:monospace 9",
            "-s",
            "/tmp/i3.sock",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        match parse_args(argv) {
            ParseOutcome::Run(o) => {
                assert_eq!(o.format, "workspace \"%s\"");
                assert_eq!(o.limit, 5);
                assert_eq!(o.prompt.as_deref(), Some("Switch: "));
                assert_eq!(o.pattern.as_deref(), Some("pango:monospace 9"));
                assert_eq!(o.socket_path.as_deref(), Some("/tmp/i3.sock"));
            }
            _ => panic!("expected Run"),
        }

        let argv: Vec<OsString> = ["i3-input", "-p", "exec "]
            .iter()
            .map(OsString::from)
            .collect();
        match parse_args(argv) {
            ParseOutcome::Run(o) => assert_eq!(o.format, "exec %s"),
            _ => panic!("expected Run"),
        }

        let argv: Vec<OsString> = ["i3-input", "-v"].iter().map(OsString::from).collect();
        assert!(matches!(parse_args(argv), ParseOutcome::Version));
        let argv: Vec<OsString> = ["i3-input", "-h"].iter().map(OsString::from).collect();
        assert!(matches!(parse_args(argv), ParseOutcome::Help));
    }
}
