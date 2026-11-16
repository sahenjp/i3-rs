//! C: i3-nagbar/main.c (665 lines) — a utility which displays a nag message,
//! for example in the case when there is an error in the configuration file.
//!
//! The layout code (button_draw/handle_expose) and the button semantics are
//! written against the NagbarRenderer seam so that tests can record the drawing
//! calls and compare them 1:1 with C. The production implementation of that
//! seam ([Libi3Renderer]) forwards to libi3's draw_util/font layer, which draws
//! into a cairo image surface and pushes it to the window with put_image (see
//! the libi3 draw_util module comment); the X11 half (window creation, randr
//! positioning, the event loop) is the rest of main.c.

#![allow(non_camel_case_types, non_snake_case)]

use libi3::draw_util::color_t;
use libi3::string::{i3string_from_utf8, i3String};

/* C: i3-nagbar/main.c:40-45 — the padding constants; the C macros call
 * logical_px() at every use site, the port asks the renderer for the value. */
pub const MSG_PADDING_LOGICAL: i32 = 8;
pub const BTN_PADDING_LOGICAL: i32 = 3;
pub const BTN_BORDER_LOGICAL: i32 = 3;
pub const BTN_GAP_LOGICAL: i32 = 20;
pub const CLOSE_BTN_GAP_LOGICAL: i32 = 15;
pub const BAR_BORDER_LOGICAL: i32 = 2;

/// C: <err.h> EXIT_FAILURE
pub const EXIT_FAILURE: i32 = 1;
/// C: paths.h _PATH_BSHELL
pub const PATH_BSHELL: &str = "/bin/sh";

/// C: i3-nagbar/main.c:55-61 typedef struct { i3String *label; char *action;
/// int16_t x; uint16_t width; bool terminal; } button_t
pub struct button_t {
    /// C: i3String *label
    pub label: i3String,
    /// C: char *action
    pub action: String,
    /// C: int16_t x
    pub x: i16,
    /// C: uint16_t width
    pub width: u16,
    /// C: bool terminal
    pub terminal: bool,
}

/// C: i3-nagbar/main.c:73-78 — Result of get_colorpixel() for the various
/// colors.
#[derive(Clone, Copy, Default)]
pub struct colors_t {
    /// C: color_background — background of the bar
    pub color_background: color_t,
    /// C: color_button_background — background for buttons
    pub color_button_background: color_t,
    /// C: color_border — color of the button border
    pub color_border: color_t,
    /// C: color_border_bottom — color of the bottom border
    pub color_border_bottom: color_t,
    /// C: color_text — color of the text
    pub color_text: color_t,
}

/// C: i3-nagbar/main.c:410-414 enum { TYPE_ERROR = 0, TYPE_WARNING = 1 }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum bar_type {
    TYPE_ERROR = 0,
    TYPE_WARNING = 1,
}

/// The drawing surface of the bar (C: surface_t bar plus the font).
pub trait NagbarRenderer {
    /// C: libi3/dpi.c:85 int logical_px(const int logical)
    fn logical_px(&mut self, logical: i32) -> i32;
    /// C: libi3/draw_util.c:163 color_t draw_util_hex_to_color(const char *color)
    fn hex_to_color(&mut self, color: &str) -> color_t;
    /// C: libi3/draw_util.c:283 void draw_util_clear_surface(surface_t *surface, color_t color)
    fn clear_surface(&mut self, color: color_t);
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
    /// C: libi3/font.c:466 int predict_text_width(i3String *text)
    fn predict_text_width(&mut self, text: &mut i3String) -> i32;
    /// C: font.height
    fn font_height(&self) -> i32;
    /// C: bar.width
    fn surface_width(&self) -> i32;
    /// C: bar.height
    fn surface_height(&self) -> i32;
    /// C: xcb_flush(conn)
    fn flush(&mut self);
}

/// C: i3-nagbar/main.c:63-71 — the file-static state of the nagbar.
pub struct Nagbar {
    /// C: xcb_window_t win (the id is only needed by the X code)
    pub win: u32,
    /// C: i3String *prompt
    pub prompt: i3String,
    /// C: button_t btn_close
    pub btn_close: button_t,
    /// C: button_t *buttons; int buttoncnt
    pub buttons: Vec<button_t>,
    /// C: the colors
    pub colors: colors_t,
    /// C: bar (surface_t)
    pub bar_width: i32,
    pub bar_height: i32,
}

impl Nagbar {
    /// C: i3-nagbar/main.c:474 btn_close.label = i3string_from_utf8("X");
    pub fn new(prompt: &str) -> Nagbar {
        Nagbar {
            win: 0,
            prompt: i3string_from_utf8(prompt),
            btn_close: button_t {
                label: i3string_from_utf8("X"),
                action: String::new(),
                x: 0,
                width: 0,
                terminal: false,
            },
            buttons: Vec::new(),
            colors: colors_t::default(),
            bar_width: 0,
            bar_height: 0,
        }
    }

    /// C: i3-nagbar/main.c:496-510 — the red theme for errors, the yellowish
    /// theme for warnings.
    pub fn set_theme(&mut self, renderer: &mut dyn NagbarRenderer, bar_type: bar_type) {
        if bar_type == bar_type::TYPE_ERROR {
            /* Red theme for error messages */
            self.colors.color_button_background = renderer.hex_to_color("#680a0a");
            self.colors.color_background = renderer.hex_to_color("#900000");
            self.colors.color_text = renderer.hex_to_color("#ffffff");
            self.colors.color_border = renderer.hex_to_color("#d92424");
            self.colors.color_border_bottom = renderer.hex_to_color("#470909");
        } else {
            /* Yellowish theme for warnings */
            self.colors.color_button_background = renderer.hex_to_color("#ffc100");
            self.colors.color_background = renderer.hex_to_color("#ffa800");
            self.colors.color_text = renderer.hex_to_color("#000000");
            self.colors.color_border = renderer.hex_to_color("#ab7100");
            self.colors.color_border_bottom = renderer.hex_to_color("#ab7100");
        }
    }
}

/// C: i3-nagbar/main.c:128 static button_t *get_button_at(int16_t x, int16_t y)
///
/// Returns the index of the button at the given coordinates (C returns a
/// pointer, or NULL).
pub fn get_button_at(buttons: &[button_t], x: i16, _y: i16) -> Option<usize> {
    for (c, button) in buttons.iter().enumerate() {
        if x >= button.x && x <= button.x + button.width as i16 {
            return Some(c);
        }
    }

    None
}

/// C: i3-nagbar/main.c:215 static int button_draw(button_t *button, int position)
///
/// Draws a button and returns its width.
pub fn button_draw(
    button: &mut button_t,
    position: i32,
    renderer: &mut dyn NagbarRenderer,
    colors: &colors_t,
) -> i32 {
    let msg_padding = renderer.logical_px(MSG_PADDING_LOGICAL);
    let btn_padding = renderer.logical_px(BTN_PADDING_LOGICAL);
    let btn_border = renderer.logical_px(BTN_BORDER_LOGICAL);
    let font_height = renderer.font_height();

    let text_width = renderer.predict_text_width(&mut button.label);
    button.width = (text_width + 2 * btn_padding + 2 * btn_border) as u16;
    button.x = (position - button.width as i32) as i16;

    /* draw border */
    renderer.rectangle(
        colors.color_border,
        (position - button.width as i32) as f64,
        (msg_padding - btn_padding - btn_border) as f64,
        button.width as f64,
        (font_height + 2 * btn_padding + 2 * btn_border) as f64,
    );
    /* draw background */
    renderer.rectangle(
        colors.color_button_background,
        (position - button.width as i32 + btn_border) as f64,
        (msg_padding - btn_padding) as f64,
        (text_width + 2 * btn_padding) as f64,
        (font_height + 2 * btn_padding) as f64,
    );
    /* draw label */
    renderer.text(
        &mut button.label,
        colors.color_text,
        colors.color_button_background,
        position - button.width as i32 + btn_border + btn_padding,
        msg_padding,
        200,
    );
    button.width as i32
}

/// C: i3-nagbar/main.c:245 static int handle_expose(xcb_connection_t *conn)
///
/// Handles expose events (redraws of the window) and rendering in general.
pub fn handle_expose(nagbar: &mut Nagbar, renderer: &mut dyn NagbarRenderer) -> i32 {
    let msg_padding = renderer.logical_px(MSG_PADDING_LOGICAL);
    let btn_padding = renderer.logical_px(BTN_PADDING_LOGICAL);
    let btn_border = renderer.logical_px(BTN_BORDER_LOGICAL);
    let btn_gap = renderer.logical_px(BTN_GAP_LOGICAL);
    let close_btn_gap = renderer.logical_px(CLOSE_BTN_GAP_LOGICAL);
    let bar_border = renderer.logical_px(BAR_BORDER_LOGICAL);

    let colors = nagbar.colors;
    let bar_width = nagbar.bar_width;
    let bar_height = nagbar.bar_height;

    /* draw background */
    renderer.clear_surface(colors.color_background);
    /* draw message */
    renderer.text(
        &mut nagbar.prompt,
        colors.color_text,
        colors.color_background,
        msg_padding,
        msg_padding,
        bar_width - 2 * msg_padding,
    );

    let mut position = bar_width - (msg_padding - btn_border - btn_padding);

    /* render close button */
    position -= button_draw(&mut nagbar.btn_close, position, renderer, &colors);
    position -= close_btn_gap;

    /* render custom buttons */
    for i in 0..nagbar.buttons.len() {
        position -= btn_gap;
        let width = button_draw(&mut nagbar.buttons[i], position, renderer, &colors);
        position -= width;
    }

    /* border line at the bottom */
    renderer.rectangle(
        colors.color_border_bottom,
        0.0,
        (bar_height - bar_border) as f64,
        bar_width as f64,
        bar_border as f64,
    );

    renderer.flush();
    1
}

/// C: i3-nagbar/main.c:138 static void handle_button_press(...)
///
/// C only prints the coordinates and leaves the re-render to the future.
pub fn handle_button_press(event_x: i16, event_y: i16) {
    println!("button pressed on x = {}, y = {}", event_x, event_y);
}

/// C: i3-nagbar/main.c:181 fprintf(script, "#!%s\nrm %s\n%s", _PATH_BSHELL,
/// script_path, button->action)
pub fn nagbar_script_content(script_path: &str, action: &str) -> String {
    format!("#!/bin/sh\nrm {}\n{}", script_path, action)
}

/// C: i3-nagbar/main.c:192-197 — the command handed to start_application().
pub fn terminal_command(link_path: &str, terminal: bool) -> String {
    if terminal {
        format!("i3-sensible-terminal -e {}", link_path)
    } else {
        link_path.to_string()
    }
}

/// What a button release does (C does it inline in handle_button_release).
#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// C: exit(0) — the close button was hit.
    Exited,
    /// A custom button was hit and its action was started.
    Launched,
    /// The pointer was not over any button.
    Ignored,
}

/// C: i3-nagbar/main.c:149 static void handle_button_release(xcb_connection_t
/// *conn, xcb_button_release_event_t *event)
///
/// The interaction part: the close button exits, a custom button runs its
/// action. The actual script creation + launch is perform_release().
pub fn button_release_target(nagbar: &Nagbar, x: i16, y: i16) -> ReleaseTarget {
    println!("button released on x = {}, y = {}", x, y);
    /* C: If the user hits the close button, we exit(0) */
    if x >= nagbar.btn_close.x && x < nagbar.btn_close.x + nagbar.btn_close.width as i16 {
        return ReleaseTarget::Exit;
    }
    match get_button_at(&nagbar.buttons, x, y) {
        Some(index) => ReleaseTarget::Button(index),
        None => ReleaseTarget::None,
    }
}

/// The target of a button release (C: the pointer to a button_t, or the
/// exit(0) of the close button).
#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseTarget {
    Exit,
    Button(usize),
    None,
}

/// C: i3-nagbar/main.c:113 static void start_application(const char *command)
///
/// Starts the given application by passing it through a shell. We use double
/// fork to avoid zombie processes.
pub fn start_application(command: &str) {
    println!("executing: {}", command);
    unsafe {
        if libc::fork() == 0 {
            /* Child process */
            libc::setsid();
            if libc::fork() == 0 {
                /* This is the child */
                let shell = std::ffi::CString::new(PATH_BSHELL).unwrap();
                let c_command = std::ffi::CString::new(command).unwrap();
                let c_flag = std::ffi::CString::new("-c").unwrap();
                libc::execl(
                    shell.as_ptr(),
                    shell.as_ptr(),
                    c_flag.as_ptr(),
                    c_command.as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
                let e = std::io::Error::last_os_error();
                eprintln!(
                    "{}: execl return: {}",
                    program_short_name(),
                    e
                );
                libc::_exit(EXIT_FAILURE);
            }
            libc::_exit(0);
        }
        libc::wait(std::ptr::null_mut());
    }
}

/// C: <err.h> program_invocation_short_name — used by the err() messages.
fn program_short_name() -> String {
    match std::env::args().next() {
        Some(arg0) => match arg0.rsplit('/').next() {
            Some(base) if !base.is_empty() => base.to_string(),
            _ => arg0,
        },
        None => "i3-nagbar".to_string(),
    }
}

/// C: i3-nagbar/main.c:161-206 — the script creation, the symlink kludge and
/// the launch of the action.
pub fn perform_release(button: &button_t, argv0: &str) {
    /* C: We need to create a custom script containing our actual command since
     * not every terminal emulator which is contained in i3-sensible-terminal
     * supports -e with multiple arguments. */
    let script_path = match libi3::get_process_filename("nagbar-cmd") {
        Some(p) => p,
        None => return,
    };

    let content = nagbar_script_content(&script_path, &button.action);
    let c_path = match std::ffi::CString::new(script_path.clone()) {
        Ok(p) => p,
        Err(_) => return,
    };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
            libc::S_IRUSR | libc::S_IWUSR,
        )
    };
    if fd == -1 {
        eprintln!(
            "{}: Could not create temporary script to store the nagbar command: {}",
            program_short_name(),
            std::io::Error::last_os_error()
        );
        return;
    }
    // C: FILE *script = fdopen(fd, "w"); fprintf(script, ...); fclose(script);
    // (written with write(2) and closed, same bytes, same "closes fd" effect)
    let bytes = content.as_bytes();
    let mut written = 0usize;
    while written < bytes.len() {
        let n = unsafe {
            libc::write(
                fd,
                bytes[written..].as_ptr() as *const libc::c_void,
                bytes.len() - written,
            )
        };
        if n <= 0 {
            break;
        }
        written += n as usize;
    }
    unsafe {
        libc::close(fd);
    }

    // C: char *link_path; sasprintf(&link_path, "%s.nagbar_cmd", script_path);
    let link_path = format!("{}.nagbar_cmd", script_path);
    // C: char *exe_path = get_exe_path(argv0); symlink(exe_path, link_path)
    let exe_path = libi3::get_exe_path(argv0);
    let c_link = std::ffi::CString::new(link_path.clone()).unwrap_or_default();
    let c_exe = std::ffi::CString::new(exe_path.clone()).unwrap_or_default();
    if unsafe { libc::symlink(c_exe.as_ptr(), c_link.as_ptr()) } == -1 {
        libi3::err(
            EXIT_FAILURE,
            &format!("Failed to symlink {} to {}", link_path, exe_path),
        );
    }

    // C: char *terminal_cmd = button->terminal ? "i3-sensible-terminal -e
    //     <link>" : <link>;
    let terminal_cmd = terminal_command(&link_path, button.terminal);
    println!("argv0 = {}", argv0);
    println!("terminal_cmd = {}", terminal_cmd);

    start_application(&terminal_cmd);
}

/// C: the full handle_button_release() (target selection + action).
pub fn handle_button_release(
    nagbar: &Nagbar,
    x: i16,
    y: i16,
    argv0: &str,
) -> ReleaseOutcome {
    match button_release_target(nagbar, x, y) {
        ReleaseTarget::Exit => ReleaseOutcome::Exited,
        ReleaseTarget::None => ReleaseOutcome::Ignored,
        ReleaseTarget::Button(index) => {
            perform_release(&nagbar.buttons[index], argv0);
            ReleaseOutcome::Launched
        }
    }
}

/// The command line of i3-nagbar (C: the variables of main()).
pub struct Options {
    /// C: char *pattern = sstrdup("pango:monospace 8")
    pub pattern: String,
    /// C: enum { TYPE_ERROR, TYPE_WARNING } bar_type
    pub bar_type: bar_type,
    /// C: bool position_on_primary
    pub position_on_primary: bool,
    /// C: buttons/buttoncnt — the label as given, the action is the next argv
    /// element.
    pub buttons: Vec<(String, String, bool)>,
    /// C: i3String *prompt
    pub prompt: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            pattern: "pango:monospace 8".to_string(),
            bar_type: bar_type::TYPE_ERROR,
            position_on_primary: false,
            buttons: Vec::new(),
            // C: i3-nagbar/main.c:429
            prompt: "Please do not run this program.".to_string(),
        }
    }
}

pub enum ParseOutcome {
    Version,
    Help,
    Run(Options),
}

/// C: i3-nagbar/main.c:416-425 static struct option long_options[]
fn long_options() -> Vec<libi3::getopt::option> {
    use libi3::getopt::{no_argument, required_argument, option};
    vec![
        option { name: "version", has_arg: no_argument, val: b'v' },
        option { name: "font", has_arg: required_argument, val: b'f' },
        option { name: "button", has_arg: required_argument, val: b'b' },
        option { name: "button-no-terminal", has_arg: required_argument, val: b'B' },
        option { name: "help", has_arg: no_argument, val: b'h' },
        option { name: "message", has_arg: required_argument, val: b'm' },
        option { name: "type", has_arg: required_argument, val: b't' },
        option { name: "primary", has_arg: no_argument, val: b'p' },
    ]
}

/// C: i3-nagbar/main.c:431-472 — the getopt_long loop.
pub fn parse_args(argv: Vec<std::ffi::OsString>) -> ParseOutcome {
    use libi3::getopt::{getopt_long, Getopt};

    let mut go = Getopt::new(argv);
    let opts = long_options();
    // C: char *options_string = "b:B:f:m:t:vhp";
    let options_string = "b:B:f:m:t:vhp";

    let mut parsed = Options::default();

    loop {
        let o = getopt_long(&mut go, options_string, &opts);
        if o == -1 {
            break;
        }
        let optarg = || go.optarg.as_ref().map(|a| a.to_string_lossy().into_owned());
        if o == b'v' as i32 {
            return ParseOutcome::Version;
        } else if o == b'f' as i32 {
            parsed.pattern = optarg().unwrap_or_default();
        } else if o == b'm' as i32 {
            parsed.prompt = optarg().unwrap_or_default();
        } else if o == b't' as i32 {
            // C: bar_type = (strcasecmp(optarg, "warning") == 0 ? TYPE_WARNING : TYPE_ERROR);
            let value = optarg().unwrap_or_default();
            parsed.bar_type = if libi3::strcasecmp(&value, "warning") == 0 {
                bar_type::TYPE_WARNING
            } else {
                bar_type::TYPE_ERROR
            };
        } else if o == b'h' as i32 {
            return ParseOutcome::Help;
        } else if o == b'p' as i32 {
            parsed.position_on_primary = true;
        } else if o == b'b' as i32 || o == b'B' as i32 {
            // C: buttons[buttoncnt].action = argv[optind];
            let label = optarg().unwrap_or_default();
            let action = go
                .argv(go.optind())
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default();
            let terminal = o == b'b' as i32;
            println!("button with label *{}* and action *{}*", label, action);
            parsed.buttons.push((label, action, terminal));
            println!("now {} buttons", parsed.buttons.len());
            // C: if (optind < argc) optind++;
            if go.optind() < go.argc() {
                let next = go.optind() + 1;
                go.set_optind(next);
            }
        }
    }

    ParseOutcome::Run(parsed)
}

/// C: i3-nagbar/main.c:435 printf("i3-nagbar " I3_VERSION "\n")
pub fn print_version() {
    print!("i3-nagbar {}\n", libi3::i3_version());
}

/// C: i3-nagbar/main.c:450-451
pub fn print_help() {
    print_version();
    print!("i3-nagbar [-m <message>] [-b <button> <action>] [-B <button> <action>] [-t warning|error] [-f <font>] [-v] [-p]\n");
}

/// C: i3-nagbar/main.c:374 int main(int argc, char *argv[]) — the argv[0]
/// kludge: when started as a *.nagbar_cmd symlink, run /bin/sh on the script.
///
/// Returns Some(cmd) when the process should exec /bin/sh cmd instead of
/// starting the nagbar (the C code execl()s right there; the port does the
/// same, this split only makes it testable).
pub fn nagbar_cmd_kludge(argv0: &str) -> Option<String> {
    const SUFFIX: &str = ".nagbar_cmd";
    if argv0.len() > SUFFIX.len() && argv0.ends_with(SUFFIX) {
        // C: unlink(argv[0]);
        let c_argv0 = std::ffi::CString::new(argv0).ok()?;
        unsafe {
            libc::unlink(c_argv0.as_ptr());
        }
        let cmd = argv0[..argv0.len() - SUFFIX.len()].to_string();
        return Some(cmd);
    }
    None
}

/// C: i3-nagbar/main.c:404 execl(_PATH_BSHELL, _PATH_BSHELL, cmd, NULL)
pub fn exec_shell(cmd: &str) -> ! {
    let shell = std::ffi::CString::new(PATH_BSHELL).unwrap();
    let c_cmd = std::ffi::CString::new(cmd).unwrap();
    unsafe {
        libc::execl(
            shell.as_ptr(),
            shell.as_ptr(),
            c_cmd.as_ptr(),
            std::ptr::null::<libc::c_char>(),
        );
    }
    // C: err(EXIT_FAILURE, "execl(%s, %s, %s)", ...) — err() never returns.
    libi3::err(EXIT_FAILURE, &format!("execl({}, {}, {})", PATH_BSHELL, PATH_BSHELL, cmd))
}

/// C: i3-nagbar/main.c:374 int main(int argc, char *argv[])
pub fn run_with_args(argv: Vec<std::ffi::OsString>) -> i32 {
    // C: the argv[0] kludge happens before anything else.
    if let Some(arg0) = argv.first() {
        let arg0 = arg0.to_string_lossy().into_owned();
        if let Some(cmd) = nagbar_cmd_kludge(&arg0) {
            exec_shell(&cmd);
        }
    }

    let argv0 = argv
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();

    match parse_args(argv) {
        ParseOutcome::Version => {
            print_version();
            return 0;
        }
        ParseOutcome::Help => {
            print_help();
            return 0;
        }
        ParseOutcome::Run(opts) => return run_nagbar(opts, &argv0),
    }
}

/* ---------------------------------------------------------------------------
 * The X11 half (C: i3-nagbar/main.c:476-664). Needs a real X server and the
 * cairo/pango layer of libi3, which is still TODO in this port.
 * ------------------------------------------------------------------------- */

/// The production renderer forwarding to libi3 (cairo/pango).
pub struct Libi3Renderer<'a> {
    pub conn: &'a x11rb::rust_connection::RustConnection,
    pub font: libi3::font::i3Font,
    pub bar_width: i32,
    pub bar_height: i32,
    /// C: the surface_t the drawing calls write to (surface_t::none() means
    /// "not initialized yet", which the draw_util_* functions skip).
    pub surface: libi3::draw_util::surface_t,
}

impl<'a> NagbarRenderer for Libi3Renderer<'a> {
    fn logical_px(&mut self, logical: i32) -> i32 {
        libi3::dpi::logical_px(logical)
    }
    fn hex_to_color(&mut self, color: &str) -> color_t {
        libi3::draw_util::draw_util_hex_to_color(color)
    }
    fn clear_surface(&mut self, color: color_t) {
        libi3::draw_util::draw_util_clear_surface(self.conn, &mut self.surface, color);
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
    fn predict_text_width(&mut self, text: &mut i3String) -> i32 {
        libi3::font::predict_text_width(Some(self.conn), text)
    }
    fn font_height(&self) -> i32 {
        self.font.height
    }
    fn surface_width(&self) -> i32 {
        self.bar_width
    }
    fn surface_height(&self) -> i32 {
        self.bar_height
    }
    fn flush(&mut self) {
        use x11rb::connection::Connection;
        let _ = self.conn.flush();
    }
}

/// C: i3-nagbar/main.c:514 — the window rectangle used when neither the
/// focused output nor the primary output can be determined:
/// `(xcb_rectangle_t){50, 50, 500, font.height + 2 * MSG_PADDING + BAR_BORDER}`.
fn default_window_rect(font_height: i32) -> (i16, i16, i32, i32) {
    let msg_padding = libi3::dpi::logical_px(MSG_PADDING_LOGICAL);
    let bar_border = libi3::dpi::logical_px(BAR_BORDER_LOGICAL);
    (
        50,
        50,
        500,
        font_height + 2 * msg_padding + bar_border,
    )
}

/// C: i3-nagbar/main.c:275 static void set_window_position_primary(xcb_rectangle_t *result)
fn set_window_position_primary(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
    result: &mut (i16, i16, i32, i32),
) {
    use x11rb::protocol::randr::ConnectionExt as RandrExt;
    let primary = conn
        .randr_get_output_primary(root)
        .ok()
        .and_then(|c| c.reply().ok());
    let res = conn
        .randr_get_screen_resources_current(root)
        .ok()
        .and_then(|c| c.reply().ok());

    let (primary, res) = match (primary, res) {
        (Some(p), Some(r)) => (p, r),
        _ => {
            println!("Could not determine the primary output.");
            return;
        }
    };

    // C: find the output with output == primary->output, then its crtc info.
    let mut crtc_id = None;
    for output in res.outputs.iter() {
        if *output == primary.output {
            if let Ok(cookie) = conn.randr_get_output_info(*output, res.config_timestamp) {
                if let Ok(info) = cookie.reply() {
                    if info.crtc != 0 {
                        crtc_id = Some(info.crtc);
                    }
                }
            }
        }
    }
    let crtc_id = match crtc_id {
        Some(c) => c,
        None => {
            println!("Could not get CRTC.");
            return;
        }
    };
    let crtc = match conn
        .randr_get_crtc_info(crtc_id, res.config_timestamp)
        .ok()
        .and_then(|c| c.reply().ok())
    {
        Some(c) => c,
        None => {
            println!("Could not get CRTC.");
            return;
        }
    };

    println!(
        "Found primary output on position x = {} / y = {} / w = {} / h = {}.",
        crtc.x, crtc.y, crtc.width, crtc.height
    );
    if crtc.width == 0 || crtc.height == 0 {
        println!("Primary output is not active, ignoring it.");
        return;
    }

    result.0 = crtc.x;
    result.1 = crtc.y;
}

/// C: i3-nagbar/main.c:330 static void set_window_position_focus(xcb_rectangle_t *result)
fn set_window_position_focus(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
    result: &mut (i16, i16, i32, i32),
) {
    use x11rb::protocol::xproto::ConnectionExt;

    // C: xcb_grab_server(conn);
    let _ = conn.grab_server();
    let mut success = false;

    if let Ok(cookie) = conn.get_input_focus() {
        if let Ok(input_focus) = cookie.reply() {
            if input_focus.focus != x11rb::NONE {
                let geometry = conn
                    .get_geometry(input_focus.focus)
                    .ok()
                    .and_then(|c| c.reply().ok());
                if let Some(geometry) = geometry {
                    let coordinates = conn
                        .translate_coordinates(
                            input_focus.focus,
                            root,
                            geometry.x,
                            geometry.y,
                        )
                        .ok()
                        .and_then(|c| c.reply().ok());
                    if let Some(coordinates) = coordinates {
                        println!(
                            "Found current focus at x = {} / y = {}.",
                            coordinates.dst_x, coordinates.dst_y
                        );
                        result.0 = coordinates.dst_x;
                        result.1 = coordinates.dst_y;
                        success = true;
                    } else {
                        println!("Failed to translate coordinates.");
                    }
                } else {
                    println!("Failed to received window geometry.");
                }
            } else {
                println!("Failed to receive the current input focus or no window has the input focus right now.");
            }
        }
    }

    // C: xcb_ungrab_server(conn);
    let _ = conn.ungrab_server();

    if !success {
        println!("Could not position on focused output, trying to position on primary output.");
        set_window_position_primary(conn, root, result);
    }
}

/// C: i3-nagbar/main.c:476-664 — the X11 half of the nagbar.
fn run_nagbar(opts: Options, argv0: &str) -> i32 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as WrapperExt;

    let (conn, screens) = match x11rb::rust_connection::RustConnection::connect(None) {
        Ok(c) => c,
        Err(_) => libi3::errx(EXIT_FAILURE, "Cannot open display"),
    };

    let root_screen = match conn.setup().roots.get(screens) {
        Some(s) => s,
        None => libi3::errx(EXIT_FAILURE, "Cannot open display"),
    };
    let root = root_screen.root;

    let mut nagbar = Nagbar::new(&opts.prompt);
    for (label, action, terminal) in &opts.buttons {
        nagbar.buttons.push(button_t {
            label: i3string_from_utf8(label),
            action: action.clone(),
            x: 0,
            width: 0,
            terminal: *terminal,
        });
    }

    // C: the colors are set before init_dpi()/load_font().
    // The theme needs the renderer, which needs the font; C sets the colors
    // with draw_util_hex_to_color() before the font is loaded, so the color
    // conversion happens on a renderer with a dummy font here as well.
    libi3::dpi::init_dpi(Some(&conn));
    let font = libi3::font::load_font(Some(&conn), &opts.pattern, true);
    libi3::font::set_font(&font);

    // C: xcb_rectangle_t win_pos = (xcb_rectangle_t){50, 50, 500,
    //    font.height + 2 * MSG_PADDING + BAR_BORDER};
    let mut win_pos = default_window_rect(font.height);
    if opts.position_on_primary {
        set_window_position_primary(&conn, root, &mut win_pos);
    } else {
        set_window_position_focus(&conn, root, &mut win_pos);
    }

    let win = match conn.generate_id() {
        Ok(w) => w,
        Err(_) => libi3::errx(EXIT_FAILURE, "Cannot generate window id"),
    };
    nagbar.win = win;
    nagbar.bar_width = win_pos.2;
    nagbar.bar_height = win_pos.3;

    // C: xcb_cursor_context_new + xcb_cursor_load_cursor(cursor_ctx, "left_ptr")
    // x11rb has no xcb-util-cursor port; the default cursor from the cursor
    // font is used instead (see TODO in the crate docs).
    // C: xcb_cursor_context_new() + xcb_cursor_load_cursor(cursor_ctx,
    // "left_ptr"). xcb-util-cursor is not ported; the glyph cursor of the
    // "cursor" font is the same X cursor (XC_left_ptr = 68).
    let cursor = match conn.generate_id() {
        Ok(c) => c,
        Err(_) => libi3::errx(EXIT_FAILURE, "Cannot generate cursor id"),
    };
    let cursor_font = match conn.generate_id() {
        Ok(c) => c,
        Err(_) => libi3::errx(EXIT_FAILURE, "Cannot generate cursor font id"),
    };
    let _ = conn.open_font(cursor_font, b"cursor");
    let _ = conn.create_glyph_cursor(
        cursor,
        cursor_font,
        cursor_font,
        68, // XC_left_ptr
        68,
        0,
        0,
        0,
        0xffff,
        0xffff,
        0xffff,
    );

    // C: xcb_create_window(..., XCB_CW_BACK_PIXEL | XCB_CW_EVENT_MASK | XCB_CW_CURSOR, ...)
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
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE,
            )
            .cursor(cursor),
    );

    // C: xcb_map_window(conn, win);
    let _ = conn.map_window(win);

    // C: the NetWM atoms (NAGBAR_ATOMS_XMACRO)
    let atom_names = [
        "_NET_WM_WINDOW_TYPE",
        "_NET_WM_WINDOW_TYPE_DOCK",
        "_NET_WM_STRUT_PARTIAL",
        "I3_SOCKET_PATH",
        "ATOM",
        "CARDINAL",
    ];
    let mut atoms: Vec<u32> = Vec::new();
    for name in atom_names {
        match conn.intern_atom(false, name.as_bytes()) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => atoms.push(reply.atom),
                Err(_) => libi3::errx(EXIT_FAILURE, &format!("Could not get atom {}", name)),
            },
            Err(_) => libi3::errx(EXIT_FAILURE, &format!("Could not get atom {}", name)),
        }
    }

    /* Set dock mode */
    let _ = conn.change_property32(
        PropMode::REPLACE,
        win,
        atoms[0],
        atoms[4],
        &[atoms[1]],
    );

    /* Reserve some space at the top of the screen */
    let strut_partial: [u32; 12] = [
        0,                                     // left
        0,                                     // right
        (font.height + libi3::dpi::logical_px(6)) as u32, // top
        0,                                     // bottom
        0,                                     // left_start_y
        0,                                     // left_end_y
        0,                                     // right_start_y
        0,                                     // right_end_y
        0,                                     // top_start_x
        800,                                   // top_end_x
        0,                                     // bottom_start_x
        0,                                     // bottom_end_x
    ];
    let _ = conn.change_property32(
        PropMode::REPLACE,
        win,
        atoms[2],
        atoms[5],
        &strut_partial,
    );

    // C: draw_util_surface_init(conn, &bar, win, get_visualtype(root_screen),
    //    win_pos.width, win_pos.height);
    // The window copies the visual from its parent (XCB_COPY_FROM_PARENT), so
    // get_visualtype(root_screen) resolves to the root depth — the port passes
    // that depth instead of looking the visual up (see libi3's draw_util
    // module comment: the drawable's depth is what put_image() needs).
    let mut surface = libi3::draw_util::surface_t::none();
    libi3::draw_util::draw_util_surface_init(
        &conn,
        &mut surface,
        win,
        root_screen.root_depth,
        win_pos.2,
        win_pos.3,
    );

    let mut renderer = Libi3Renderer {
        conn: &conn,
        font: i3FontCopy(&font),
        bar_width: win_pos.2,
        bar_height: win_pos.3,
        surface,
    };
    nagbar.set_theme(&mut renderer, opts.bar_type);

    let _ = conn.flush();

    // C: while ((event = xcb_wait_for_event(conn)) != NULL)
    loop {
        use x11rb::protocol::Event;
        let event = match conn.wait_for_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match event {
            Event::Error(e) => {
                eprintln!("X11 Error received! sequence {:x}", e.sequence);
            }
            Event::Expose(e) => {
                if e.count == 0 {
                    handle_expose(&mut nagbar, &mut renderer);
                }
            }
            Event::ButtonPress(e) => {
                handle_button_press(e.event_x, e.event_y);
            }
            Event::ButtonRelease(e) => {
                match handle_button_release(&nagbar, e.event_x, e.event_y, argv0) {
                    ReleaseOutcome::Exited => return 0,
                    _ => {}
                }
            }
            Event::ConfigureNotify(e) => {
                // C: if (width > 0 && height > 0) draw_util_surface_set_size(&bar, ...)
                if e.width > 0 && e.height > 0 {
                    nagbar.bar_width = e.width as i32;
                    nagbar.bar_height = e.height as i32;
                    renderer.bar_width = e.width as i32;
                    renderer.bar_height = e.height as i32;
                    libi3::draw_util::draw_util_surface_set_size(
                        &mut renderer.surface,
                        e.width as i32,
                        e.height as i32,
                    );
                }
            }
            _ => {}
        }
    }

    // C: free(pattern); draw_util_surface_free(conn, &bar);
    libi3::draw_util::draw_util_surface_free(&conn, &mut renderer.surface);

    0
}

/// C: load_font() returns an i3Font by value; the renderer owns a copy.
fn i3FontCopy(font: &libi3::font::i3Font) -> libi3::font::i3Font {
    libi3::font::i3Font {
        r#type: font.r#type,
        height: font.height,
        pattern: font.pattern.clone(),
    }
}

/// C: i3-nagbar/main.c:374 int main(int argc, char *argv[])
pub fn run() -> i32 {
    run_with_args(std::env::args_os().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libi3::string::i3string_as_utf8;

    #[derive(Debug, PartialEq)]
    enum Op {
        Clear(&'static str),
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
                height: 24,
                font_height: 16,
                char_width: 8,
            }
        }
    }

    fn color_of(name: &str) -> color_t {
        let mut c = color_t::default();
        c.colorpixel = match name {
            "#680a0a" => 0x680a0a,
            "#900000" => 0x900000,
            "#ffffff" => 0xffffff,
            "#d92424" => 0xd92424,
            "#470909" => 0x470909,
            "#ffc100" => 0xffc100,
            "#ffa800" => 0xffa800,
            "#000000" => 0x000000,
            "#ab7100" => 0xab7100,
            _ => 0x123456,
        };
        c
    }

    fn color_name(color: color_t) -> &'static str {
        match color.colorpixel {
            0x680a0a => "#680a0a",
            0x900000 => "#900000",
            0xffffff => "#ffffff",
            0xd92424 => "#d92424",
            0x470909 => "#470909",
            0xffc100 => "#ffc100",
            0xffa800 => "#ffa800",
            0x000000 => "#000000",
            0xab7100 => "#ab7100",
            _ => "#123456",
        }
    }

    impl NagbarRenderer for RecordingRenderer {
        fn logical_px(&mut self, logical: i32) -> i32 {
            logical * self.dpi_scale
        }
        fn hex_to_color(&mut self, color: &str) -> color_t {
            color_of(color)
        }
        fn clear_surface(&mut self, color: color_t) {
            self.ops.push(Op::Clear(color_name(color)));
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
            let s = i3string_as_utf8(text).unwrap_or("").to_string();
            self.ops.push(Op::Text {
                text: s,
                x,
                y,
                max_width,
            });
        }
        fn predict_text_width(&mut self, text: &mut i3String) -> i32 {
            i3string_as_utf8(text).unwrap_or("").chars().count() as i32 * self.char_width
        }
        fn font_height(&self) -> i32 {
            self.font_height
        }
        fn surface_width(&self) -> i32 {
            self.width
        }
        fn surface_height(&self) -> i32 {
            self.height
        }
        fn flush(&mut self) {
            self.ops.push(Op::Flush);
        }
    }

    fn test_nagbar() -> (Nagbar, RecordingRenderer) {
        let mut nagbar = Nagbar::new("Please do not run this program.");
        let mut renderer = RecordingRenderer::new();
        nagbar.set_theme(&mut renderer, bar_type::TYPE_ERROR);
        nagbar.bar_width = 500;
        nagbar.bar_height = 24;
        (nagbar, renderer)
    }

    #[test]
    fn theme_colors_match_c() {
        // C: i3-nagbar/main.c:496-510
        let (nagbar, _r) = test_nagbar();
        assert_eq!(color_name(nagbar.colors.color_button_background), "#680a0a");
        assert_eq!(color_name(nagbar.colors.color_background), "#900000");
        assert_eq!(color_name(nagbar.colors.color_text), "#ffffff");
        assert_eq!(color_name(nagbar.colors.color_border), "#d92424");
        assert_eq!(color_name(nagbar.colors.color_border_bottom), "#470909");

        let mut nagbar = Nagbar::new("warning");
        let mut renderer = RecordingRenderer::new();
        nagbar.set_theme(&mut renderer, bar_type::TYPE_WARNING);
        assert_eq!(color_name(nagbar.colors.color_button_background), "#ffc100");
        assert_eq!(color_name(nagbar.colors.color_background), "#ffa800");
        assert_eq!(color_name(nagbar.colors.color_text), "#000000");
        assert_eq!(color_name(nagbar.colors.color_border), "#ab7100");
        assert_eq!(color_name(nagbar.colors.color_border_bottom), "#ab7100");
    }

    #[test]
    fn expose_draws_message_close_button_and_bottom_border() {
        let (mut nagbar, mut renderer) = test_nagbar();
        assert_eq!(handle_expose(&mut nagbar, &mut renderer), 1);

        // C: msg_padding = 8, btn_padding = 3, btn_border = 3, close_gap = 15,
        // bar_border = 2, font.height = 16, "X" is 8px wide.
        // position = 500 - (8 - 3 - 3) = 498; close width = 8 + 6 + 6 = 20,
        // so x = 478, the border rectangle starts at (478, 8-3-3=2) and is
        // font.height + 2*3 + 2*3 = 28 high.
        assert_eq!(
            renderer.ops,
            vec![
                Op::Clear("#900000"),
                Op::Text { text: "Please do not run this program.".to_string(), x: 8, y: 8, max_width: 484 },
                Op::Rectangle { color: "#d92424", x: 478.0, y: 2.0, w: 20.0, h: 28.0 },
                Op::Rectangle { color: "#680a0a", x: 481.0, y: 5.0, w: 14.0, h: 22.0 },
                Op::Text { text: "X".to_string(), x: 484, y: 8, max_width: 200 },
                Op::Rectangle { color: "#470909", x: 0.0, y: 22.0, w: 500.0, h: 2.0 },
                Op::Flush,
            ]
        );
        // The button geometry is stored on the button like in C.
        assert_eq!(nagbar.btn_close.width, 20);
        assert_eq!(nagbar.btn_close.x, 478);
    }

    #[test]
    fn expose_renders_custom_buttons_after_the_close_button() {
        let (mut nagbar, mut renderer) = test_nagbar();
        nagbar.buttons.push(button_t {
            label: i3string_from_utf8("edit"),
            action: "i3-sensible-editor /tmp/x".to_string(),
            x: 0,
            width: 0,
            terminal: true,
        });
        handle_expose(&mut nagbar, &mut renderer);

        // width("edit") = 32 -> button width 32 + 6 + 6 = 44.
        // position after the close button: 498 - 20 - 15 = 463;
        // then - BTN_GAP(20) = 443, x = 443 - 44 = 399.
        let rects: Vec<&Op> = renderer
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Rectangle { .. }))
            .collect();
        assert_eq!(
            rects[2],
            &Op::Rectangle { color: "#d92424", x: 399.0, y: 2.0, w: 44.0, h: 28.0 }
        );
        assert_eq!(
            rects[3],
            &Op::Rectangle { color: "#680a0a", x: 402.0, y: 5.0, w: 38.0, h: 22.0 }
        );
        assert_eq!(nagbar.buttons[0].width, 44);
        assert_eq!(nagbar.buttons[0].x, 399);
        // The label is drawn last, with the 200px limit of C.
        assert_eq!(
            renderer.ops[7],
            Op::Text { text: "edit".to_string(), x: 405, y: 8, max_width: 200 }
        );
    }

    #[test]
    fn get_button_at_uses_the_inclusive_x_range() {
        let buttons = vec![
            button_t { label: i3string_from_utf8("a"), action: String::new(), x: 100, width: 50, terminal: false },
            button_t { label: i3string_from_utf8("b"), action: String::new(), x: 200, width: 50, terminal: false },
        ];
        assert_eq!(get_button_at(&buttons, 100, 0), Some(0));
        assert_eq!(get_button_at(&buttons, 150, 0), Some(0));
        assert_eq!(get_button_at(&buttons, 99, 0), None);
        assert_eq!(get_button_at(&buttons, 200, 0), Some(1));
        assert_eq!(get_button_at(&buttons, 250, 0), Some(1));
        assert_eq!(get_button_at(&buttons, 251, 0), None);
    }

    #[test]
    fn close_button_exits_before_the_custom_buttons_are_checked() {
        let (mut nagbar, mut renderer) = test_nagbar();
        nagbar.buttons.push(button_t {
            label: i3string_from_utf8("x"),
            action: "true".to_string(),
            x: 0,
            width: 0,
            terminal: false,
        });
        handle_expose(&mut nagbar, &mut renderer);
        // The close button covers [478, 498).
        assert_eq!(button_release_target(&nagbar, 478, 10), ReleaseTarget::Exit);
        assert_eq!(button_release_target(&nagbar, 497, 10), ReleaseTarget::Exit);
        // Over a custom button: the index comes back.
        let bx = nagbar.buttons[0].x;
        assert_eq!(
            button_release_target(&nagbar, bx, 10),
            ReleaseTarget::Button(0)
        );
        // Somewhere else: nothing.
        assert_eq!(button_release_target(&nagbar, 0, 10), ReleaseTarget::None);
    }

    #[test]
    fn script_content_and_terminal_command_match_c() {
        // C: fprintf(script, "#!%s\nrm %s\n%s", _PATH_BSHELL, script_path, action)
        assert_eq!(
            nagbar_script_content("/run/user/1000/i3/nagbar-cmd.abc", "i3-msg reload"),
            "#!/bin/sh\nrm /run/user/1000/i3/nagbar-cmd.abc\ni3-msg reload"
        );
        // C: button->terminal ? "i3-sensible-terminal -e <link>" : <link>
        assert_eq!(
            terminal_command("/tmp/x.nagbar_cmd", true),
            "i3-sensible-terminal -e /tmp/x.nagbar_cmd"
        );
        assert_eq!(terminal_command("/tmp/x.nagbar_cmd", false), "/tmp/x.nagbar_cmd");
    }

    #[test]
    fn parse_args_builds_the_buttons_like_c() {
        use std::ffi::OsString;
        // C: -b <label> <action>, -B <label> <action> (no terminal)
        let argv: Vec<OsString> = [
            "i3-nagbar",
            "-m",
            "You have an error in your config",
            "-t",
            "warning",
            "-b",
            "edit config",
            "i3-sensible-editor /tmp/bad.conf",
            "-B",
            "show errors",
            "i3-nagbar -m 'x'",
            "-f",
            "pango:monospace 9",
            "-p",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        match parse_args(argv) {
            ParseOutcome::Run(o) => {
                assert_eq!(o.prompt, "You have an error in your config");
                assert_eq!(o.bar_type, bar_type::TYPE_WARNING);
                assert_eq!(o.pattern, "pango:monospace 9");
                assert!(o.position_on_primary);
                assert_eq!(o.buttons.len(), 2);
                assert_eq!(
                    o.buttons[0],
                    (
                        "edit config".to_string(),
                        "i3-sensible-editor /tmp/bad.conf".to_string(),
                        true
                    )
                );
                assert_eq!(
                    o.buttons[1],
                    ("show errors".to_string(), "i3-nagbar -m 'x'".to_string(), false)
                );
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parse_args_defaults() {
        use std::ffi::OsString;
        let argv: Vec<OsString> = ["i3-nagbar"].iter().map(OsString::from).collect();
        match parse_args(argv) {
            ParseOutcome::Run(o) => {
                assert_eq!(o.prompt, "Please do not run this program.");
                assert_eq!(o.bar_type, bar_type::TYPE_ERROR);
                assert_eq!(o.pattern, "pango:monospace 8");
                assert!(!o.position_on_primary);
                assert!(o.buttons.is_empty());
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn unknown_type_falls_back_to_error() {
        use std::ffi::OsString;
        let argv: Vec<OsString> = ["i3-nagbar", "-t", "WARNING"]
            .iter()
            .map(OsString::from)
            .collect();
        match parse_args(argv) {
            ParseOutcome::Run(o) => assert_eq!(o.bar_type, bar_type::TYPE_WARNING),
            _ => panic!("expected Run"),
        }
        let argv: Vec<OsString> = ["i3-nagbar", "-t", "nonsense"]
            .iter()
            .map(OsString::from)
            .collect();
        match parse_args(argv) {
            ParseOutcome::Run(o) => assert_eq!(o.bar_type, bar_type::TYPE_ERROR),
            _ => panic!("expected Run"),
        }
    }

    /// C: i3-nagbar/main.c:514 — the default window geometry. The surface the
    /// renderer draws into has exactly this size (draw_util_surface_init(...,
    /// win_pos.width, win_pos.height)), which is what the real-X screenshot
    /// shows for both the C and the Rust binary (500x32 for a 14px font).
    #[test]
    fn default_window_rect_matches_c() {
        assert_eq!(default_window_rect(14), (50, 50, 500, 32));
        // font.height + 2 * 8 + 2
        assert_eq!(default_window_rect(0).3, 18);
        assert_eq!(default_window_rect(20).3, 38);
    }

    #[test]
    fn nagbar_cmd_kludge_strips_the_suffix() {
        // C: i3-nagbar/main.c:397-406 — a *.nagbar_cmd symlink is run through
        // /bin/sh on the path without the suffix (the unlink is a no-op for a
        // path that does not exist).
        assert_eq!(
            nagbar_cmd_kludge("/tmp/i3-nagbar-cmd.123.nagbar_cmd"),
            Some("/tmp/i3-nagbar-cmd.123".to_string())
        );
        assert_eq!(nagbar_cmd_kludge("/usr/bin/i3-nagbar"), None);
        // A path that is exactly the suffix is not treated as a command.
        assert_eq!(nagbar_cmd_kludge(".nagbar_cmd"), None);
    }
}
