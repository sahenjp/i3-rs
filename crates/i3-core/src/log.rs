//! C: src/log.c (436 lines) — the logging subsystem: the SHM ring buffer that
//! i3-dump-log reads, the log-file, stderr and log-stream-socket outputs, and
//! the verbosity switches.
//!
//! The ring-buffer header layout (include/shmlog.h) is an on-disk ABI shared
//! with i3-dump-log, so the byte layout is preserved exactly.

#![allow(dead_code)]

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;

/// C: include/shmlog.h — i3_shmlog_header (offset_next_write,
/// offset_last_wrap, size, wrap_count, wrap_count_last).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct i3_shmlog_header {
    pub offset_next_write: u32,
    pub offset_last_wrap: u32,
    pub size: u32,
    pub wrap_count: u32,
    pub wrap_count_last: u32,
}

/// C: src/log.c:44 char *shmlogname = "";
static SHMLOGNAME: Mutex<String> = Mutex::new(String::new());
/// C: src/log.c:47 int shmlog_size = 0;
///
/// Signed on purpose: C's "shmlog toggle" turns logging off by negating the
/// size (src/commands.c:2326), and init_logging treats any value <= 0 as
/// disabled.
static SHMLOG_SIZE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
/// C: src/log.c:206 static bool verbose = false;
static VERBOSE: AtomicBool = AtomicBool::new(false);
/// C: src/log.c:222 static bool debug_logging = false;
static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);
/// C: src/log.c:40 static FILE *errorfile;
static ERRORFILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
/// C: src/log.c:38 char *errorfilename;
static ERRORFILENAME: Mutex<Option<String>> = Mutex::new(None);
/// C: src/log.c:53 static i3_shmlog_header *header; (and the buffer)
static LOGBUFFER: OnceLock<Mutex<Option<LogBuffer>>> = OnceLock::new();

/// The mapped SHM segment (C: logbuffer / header / logwalk / loglastwrap).
pub struct LogBuffer {
    /// C: char *logbuffer — the SHARED mapping (src/log.c:172 mmap(...,
    /// MAP_SHARED, ...)). i3-dump-log reads the very same bytes through
    /// shm_open(shmlogname), so this must stay the mapping and not a copy.
    pub map: *mut u8,
    /// C: int logbuffer_size — the size of the segment actually mapped.
    pub size: usize,
    /// C: char *logwalk
    pub logwalk: usize,
    /// C: char *loglastwrap
    pub loglastwrap: usize,
    /// C: int logbuffer_shm
    pub shm_fd: i32,
    pub shm_name: String,
}

// C keeps raw pointers as well; the mapping is only touched through the
// LogBuffer mutex (logbuffer_cell), so the segment is never aliased.
unsafe impl Send for LogBuffer {}

impl LogBuffer {
    /// The mapped segment (C: the logbuffer bytes / pointer arithmetic).
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.map, self.size) }
    }

    /// C: writes into logbuffer at logwalk.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.map, self.size) }
    }

    /// C: int logbuffer_size — the mapped length.
    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}

fn logbuffer_cell() -> &'static Mutex<Option<LogBuffer>> {
    LOGBUFFER.get_or_init(|| Mutex::new(None))
}

fn errorfile_cell() -> &'static Mutex<Option<std::fs::File>> {
    ERRORFILE.get_or_init(|| Mutex::new(None))
}

/// C: src/log.c:54 SHMLOG_SIZE — the development default (25 MiB).
pub fn default_shmlog_size() -> usize {
    // C: include/shmlog.h:30 #define SHMLOG_SIZE (25 * 1024 * 1024)
    25 * 1024 * 1024
}

/// C: src/log.c:47 shmlog_size — read by init_logging/logging_enabled.
/// (C's int can go negative through "shmlog toggle"; the usize view clamps
/// that to 0, which is what every size consumer expects.)
pub fn shmlog_size() -> usize {
    SHMLOG_SIZE.load(Ordering::Relaxed).max(0) as usize
}

/// C: src/log.c:47 shmlog_size as the signed int the commands layer sees.
pub fn shmlog_size_signed() -> i64 {
    SHMLOG_SIZE.load(Ordering::Relaxed)
}

/// C: src/commands.c:2326 sets shmlog_size to -shmlog_size or to the parsed
/// value; both are signed writes of the global.
pub fn set_shmlog_size_signed(size: i64) {
    SHMLOG_SIZE.store(size, Ordering::Relaxed);
}

/// C: src/log.c:47 shmlog_size = ...
pub fn set_shmlog_size(size: usize) {
    SHMLOG_SIZE.store(size as i64, Ordering::Relaxed);
}

/// C: src/log.c:59 bool logging_enabled(void)
pub fn logging_enabled() -> bool {
    logbuffer_cell().lock().map(|g| g.is_some()).unwrap_or(false)
}

/// C: src/log.c:44 char *shmlogname ("" while disabled) — the SHM segment
/// name the X layer exports as the I3_SHMLOG_PATH root atom (src/x.c:1521
/// update_shmlog_atom) so that i3-dump-log can find the ring buffer.
pub fn shmlog_name() -> Option<String> {
    match SHMLOGNAME.lock() {
        Ok(g) if !g.is_empty() => Some(g.clone()),
        _ => None,
    }
}

/// C: src/log.c:38 char *errorfilename — the path of this process' error log
/// (C: get_process_filename("errorlog")), set by init_logging().
pub fn errorfilename() -> Option<String> {
    ERRORFILENAME.lock().ok().and_then(|g| g.clone())
}

/// C: "errorfilename = ..." — the one place the path is assigned (init_logging,
/// and the tests).
fn set_errorfilename(path: Option<String>) {
    match ERRORFILENAME.lock() {
        Ok(mut g) => *g = path,
        Err(e) => *e.into_inner() = path,
    }
}

/// C: src/log.c:365 void purge_zerobyte_logfile(void)
///
/// Removes the error log (and its directory) if it stayed empty, so that a
/// clean run leaves nothing behind. Called on exit and before restart()
/// (C: src/log.c:129 atexit, src/commands.c:1720).
pub fn purge_zerobyte_logfile() {
    let path = match errorfilename() {
        Some(p) => p,
        None => return,
    };

    /* don't delete the log file if it contains something */
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() == 0 => {}
        _ => return,
    }

    if std::fs::remove_file(&path).is_err() {
        return;
    }

    /* possibly fails with ENOTEMPTY if there are files (or sockets) left. */
    if let Some(slash) = path.rfind('/') {
        let _ = std::fs::remove_dir(&path[..slash]);
    }
}

/// C: src/log.c:129 atexit(purge_zerobyte_logfile) — the C ABI shim for the
/// atexit hook (the Rust side cannot unwrap, so a poisoned lock is ignored).
extern "C" fn purge_zerobyte_logfile_atexit() {
    purge_zerobyte_logfile();
}

/// C: src/log.c:95 void init_logging(void)
pub fn init_logging() {
    // C: atexit(purge_zerobyte_logfile); (registered once, it is idempotent)
    static ATEXIT_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !ATEXIT_DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        unsafe {
            libc::atexit(purge_zerobyte_logfile_atexit);
        }
    }

    // C: if (!errorfilename) { errorfilename = get_process_filename("errorlog");
    //     errorfile = fopen(errorfilename, "w"); ... }
    if errorfilename().is_none() {
        match libi3::get_process_filename::get_process_filename("errorlog") {
            Some(path) => {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)
                {
                    Ok(file) => {
                        if let Ok(mut f) = errorfile_cell().lock() {
                            *f = Some(file);
                        }
                    }
                    Err(e) => {
                        // C: fprintf(stderr, "Could not initialize errorlog on
                        // %s: %s\n", errorfilename, strerror(errno));
                        eprintln!("Could not initialize errorlog on {}: {}", path, e);
                    }
                }
                set_errorfilename(Some(path));
            }
            None => {
                // C: fprintf(stderr, "Could not initialize errorlog\n");
                eprintln!("Could not initialize errorlog");
            }
        }
    }

    // C: if (shmlog_size > 0 && logbuffer == NULL) open_logbuffer();
    //    else if (shmlog_size <= 0 && logbuffer) close_logbuffer();
    let size = shmlog_size_signed();
    if size > 0 && !logging_enabled() {
        shmlog_create();
    } else if size <= 0 && logging_enabled() {
        shmlog_destroy();
    }
}

/// C: src/log.c:120-199 — shmlog_create(): shm_open + ftruncate/fallocate +
/// mmap, then initialise the header.
fn shmlog_create() {
    let size = shmlog_size();
    // C: logbuffer_size = shmlog_size; if (physical_mem_bytes * 0.01 <
    //     (long long)shmlog_size) { shmlog_size = physical_mem_bytes * 0.01; }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) } as u64;
    let physical_mem_bytes = (pages as usize).saturating_mul(page);
    let capped = if (physical_mem_bytes / 100) < size {
        physical_mem_bytes / 100
    } else {
        size
    };

    // C: sasprintf(&shmlogname, "/i3-log-%d", getpid());  (Linux)
    let name = format!("/i3-log-{}", std::process::id());
    if let Ok(mut g) = SHMLOGNAME.lock() {
        *g = name.clone();
    }

    let cname = match CString::new(name.clone()) {
        Ok(c) => c,
        Err(_) => return,
    };
    // C: shm_open(shmlogname, O_RDWR | O_CREAT, S_IREAD | S_IWRITE);
    let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o600) };
    if fd == -1 {
        return;
    }
    // C: posix_fallocate(logbuffer_shm, 0, shmlog_size);
    let rc = unsafe { libc::ftruncate(fd, capped as libc::off_t) };
    if rc == -1 {
        unsafe {
            libc::close(fd);
            libc::shm_unlink(cname.as_ptr());
        }
        return;
    }
    // C: logbuffer = mmap(NULL, shmlog_size, PROT_READ | PROT_WRITE, MAP_SHARED, ...)
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            capped,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        unsafe {
            libc::close(fd);
            libc::shm_unlink(cname.as_ptr());
        }
        return;
    }
    // C: memset(logbuffer, '\0', logbuffer_size);
    let map = ptr as *mut u8;
    unsafe {
        std::ptr::write_bytes(map, 0, capped);
    }

    // C: header = (i3_shmlog_header *)logbuffer; header->size = shmlog_size;
    //     logwalk = logbuffer + sizeof(i3_shmlog_header);
    //     loglastwrap = logbuffer + shmlog_size;
    //
    // The mapping stays open (C keeps logbuffer_shm as well): the ring buffer
    // has to be the shared segment, otherwise i3-dump-log would open a
    // segment that never receives anything.
    let mut buf = LogBuffer {
        map,
        size: capped,
        logwalk: std::mem::size_of::<i3_shmlog_header>(),
        loglastwrap: capped,
        shm_fd: fd,
        shm_name: name,
    };
    let hdr = i3_shmlog_header {
        offset_next_write: buf.logwalk as u32,
        offset_last_wrap: buf.loglastwrap as u32,
        size: capped as u32,
        wrap_count: 0,
        wrap_count_last: 0,
    };
    write_header(&mut buf, &hdr);

    if let Ok(mut g) = logbuffer_cell().lock() {
        *g = Some(buf);
    }
}

fn write_header(buf: &mut LogBuffer, hdr: &i3_shmlog_header) {
    let bytes = unsafe {
        std::slice::from_raw_parts(hdr as *const i3_shmlog_header as *const u8, std::mem::size_of::<i3_shmlog_header>())
    };
    buf.as_mut_slice()[..bytes.len()].copy_from_slice(bytes);
}

fn read_header(buf: &LogBuffer) -> i3_shmlog_header {
    let mut hdr = i3_shmlog_header::default();
    let n = std::mem::size_of::<i3_shmlog_header>();
    unsafe {
        std::ptr::copy_nonoverlapping(
            buf.as_slice().as_ptr(),
            &mut hdr as *mut i3_shmlog_header as *mut u8,
            n,
        );
    }
    hdr
}

/// C: src/log.c:189-199 close_logbuffer() — close + shm_unlink + munmap.
/// Called by init_logging() when the size drops to 0 ("shmlog off") and by
/// the exit path (C: i3_exit's shm_unlink(shmlogname)).
pub fn shmlog_destroy() {
    if let Ok(mut g) = logbuffer_cell().lock() {
        if let Some(buf) = g.take() {
            // C: close_logbuffer() — close(logbuffer_shm); shm_unlink(name);
            // the mapping itself is unmapped here as well, otherwise the
            // segment would stay mapped after "shmlog off".
            if !buf.is_empty() {
                unsafe {
                    libc::munmap(buf.map as *mut libc::c_void, buf.size);
                }
            }
            if buf.shm_fd != -1 {
                unsafe {
                    libc::close(buf.shm_fd);
                }
            }
            if let Ok(cname) = CString::new(buf.shm_name) {
                unsafe {
                    libc::shm_unlink(cname.as_ptr());
                }
            }
        }
    }
    if let Ok(mut n) = SHMLOGNAME.lock() {
        *n = String::new();
    }
}

/// C: src/log.c:205 void set_verbosity(bool _verbose)
pub fn set_verbosity(v: bool) {
    VERBOSE.store(v, Ordering::Relaxed);
}

/// C: src/log.c:206 bool get_verbosity(void)
pub fn get_verbosity() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// C: src/log.c:221 void set_debug_logging(const bool _debug_logging)
pub fn set_debug_logging(v: bool) {
    DEBUG_LOGGING.store(v, Ordering::Relaxed);
}

/// C: src/log.c:222 bool get_debug_logging(void)
pub fn get_debug_logging() -> bool {
    DEBUG_LOGGING.load(Ordering::Relaxed)
}

/// C: src/log.c:232-320 — vlog(): the timestamp prefix, the 4096 byte limit,
/// the ring-buffer write, the log-file and the broadcast to log-stream
/// clients.
fn vlog(print: bool, level: &str, message: &str) -> String {
    // C: static char message[4096]; len = strftime(message, sizeof(message),
    //    "%x %X - ", tmp);
    let now = current_time_string();
    let mut full = format!("{}{}{}", now, level, message).into_bytes();

    // C: len += vsnprintf(message + len, sizeof(message) - len, fmt, args);
    //    if (len >= sizeof(message)) {
    //        fprintf(stderr, "BUG: single log message > 4k\n");
    //        len = sizeof(message);
    //        message[len - 2] = '\n';
    //    }
    if full.len() >= 4096 {
        eprintln!("BUG: single log message > 4k");
        full.truncate(4096);
        // C: message[len - 2] = '\n' — punch in a newline so the next log
        // message is not dangling after the truncated one.
        full[4094] = b'\n';
    }

    // C: the ring buffer write (only while a logbuffer is mapped).
    if let Ok(mut g) = logbuffer_cell().lock() {
        if let Some(buf) = g.as_mut() {
            let mut hdr = read_header(buf);
            let needed = full.len();
            let size = buf.size;
            // C: if (len >= (size_t)(logbuffer_size - (logwalk - logbuffer))) {
            //        loglastwrap = logwalk;
            //        logwalk = logbuffer + sizeof(i3_shmlog_header);
            //        store_log_markers(); header->wrap_count++;
            //    }
            if needed >= size.saturating_sub(buf.logwalk) {
                buf.loglastwrap = buf.logwalk;
                buf.logwalk = std::mem::size_of::<i3_shmlog_header>();
                hdr.offset_next_write = buf.logwalk as u32;
                hdr.offset_last_wrap = buf.loglastwrap as u32;
                hdr.size = size as u32;
                write_header(buf, &hdr);
                hdr.wrap_count = hdr.wrap_count.wrapping_add(1);
            }

            // C: strncpy(logwalk, message, len); logwalk += len;
            let start = buf.logwalk;
            let end = (start + needed).min(size);
            let n = end.saturating_sub(start);
            let slice = buf.as_mut_slice();
            slice[start..end].copy_from_slice(&full[..n]);
            buf.logwalk = end;

            // C: store_log_markers();
            hdr.offset_next_write = buf.logwalk as u32;
            hdr.offset_last_wrap = buf.loglastwrap as u32;
            hdr.size = size as u32;
            write_header(buf, &hdr);
        }
    }

    if print {
        {
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(&full);
        }
    }
    let text = String::from_utf8_lossy(&full).into_owned();
    // C: log_broadcast_to_clients(message, len);
    log_broadcast_to_clients(&text);
    text
}

/// C: src/log.c:249 — the time prefix, strftime("%x %X - ", localtime(t)).
/// "%x %X" is locale dependent on purpose (a Japanese locale prints
/// "2026年11月16日 07時29分28秒 - "), so the C strftime() is called directly.
fn current_time_string() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = now as libc::time_t;
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    let mut buf = [0i8; 64];
    unsafe {
        libc::strftime(
            buf.as_mut_ptr(),
            buf.len(),
            b"%x %X - \0".as_ptr() as *const libc::c_char,
            &tm,
        );
    }
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// C: include/log.h ELOG(fmt) — errorlog("ERROR: " fmt).
pub fn elog(message: &str) {
    errorlog(&format!("ERROR: {}", message));
}

/// C: include/log.h DLOG(fmt) — "%s:%s:%d - " fmt, written by the call site.
pub fn dlog_at(file: &str, function: &str, line: u32, message: &str) {
    let msg = format!("{}:{}:{} - {}", file, function, line, message);
    debuglog(&msg);
}

/// C: src/log.c:302-316 verboselog / include/log.h LOG()
///
/// C only returns early when there is no logbuffer *and* verbose is off;
/// with SHM logging enabled every LOG() reaches the ring buffer (printed to
/// stdout only when verbose). Test 207 relies on this: the "nop" command is
/// only visible in i3-dump-log because of the ring-buffer write.
pub fn log_(message: &str) {
    if !logging_enabled() && !get_verbosity() {
        return;
    }
    vlog(get_verbosity(), "", message);
}

/// C: src/log.c:325 void errorlog(char *fmt, ...) — vlog(print=true), i.e.
/// printed to **stdout**, and appended to the error file.
pub fn errorlog(message: &str) {
    // C: vlog(print=true) already writes to stdout.
    let full = vlog(true, "", message);
    if let Ok(mut f) = errorfile_cell().lock() {
        if let Some(file) = f.as_mut() {
            use std::io::Write;
            let _ = file.write_all(full.as_bytes());
        }
    }
}

/// C: src/log.c:348 void debuglog(char *fmt, ...) — DLOG, with the file and
/// line. The C version is a macro that appends "(file:line)"; callers pass
/// the location explicitly through debuglog_at().
pub fn debuglog(message: &str) {
    // C: if (!logbuffer && !(debug_logging)) { return; }
    if !logging_enabled() && !get_debug_logging() {
        return;
    }
    vlog(get_debug_logging(), "", message);
}

/// C: include/log.h DLOG macro — appends " (file:line)".
pub fn debuglog_at(file: &str, line: u32, message: &str) {
    if !logging_enabled() && !get_debug_logging() {
        return;
    }
    let msg = format!("{} ({}:{})\n", message.trim_end_matches('\n'), file, line);
    vlog(get_debug_logging(), "", &msg);
}

/// C: include/log.h LOG macro / src/log.c vlog(...) — the shmlog-aware
/// variant used by the main binary.
pub fn mainlog(message: &str) {
    vlog(true, "", message);
}

/// C: src/commands.c:2347 update_shmlog_atom() — cmd_shmlog runs in the
/// command layer, which has no X connection, so it leaves a request behind
/// and the event loop performs x_set_i3_atoms() right after the command
/// (bins/i3/src/server.rs), the same place C's synchronous call lands in.
static SHMLOG_ATOM_UPDATE: AtomicBool = AtomicBool::new(false);

/// C: src/commands.c:2347 update_shmlog_atom() call from cmd_shmlog.
pub fn request_shmlog_atom_update() {
    SHMLOG_ATOM_UPDATE.store(true, Ordering::Relaxed);
}

/// Consumes the request made by cmd_shmlog (see [request_shmlog_atom_update]).
pub fn take_shmlog_atom_update() -> bool {
    SHMLOG_ATOM_UPDATE.swap(false, Ordering::Relaxed)
}

/// C: src/log.c:390 char *current_log_stream_socket_path = NULL;
///
/// The path create_socket() actually bound (src/main.c:1017), exported as the
/// I3_LOG_STREAM_SOCKET_PATH root atom (src/x.c:1544) so that i3-dump-log -f
/// can connect, and unlinked on exit (src/main.c:188) and restart
/// (src/commands.c:1715).
static CURRENT_LOG_STREAM_SOCKET_PATH: Mutex<Option<String>> = Mutex::new(None);

/// C: src/log.c:390 — written by create_socket() out parameter
/// (src/main.c:1017) and left NULL when the socket could not be created.
pub fn set_current_log_stream_socket_path(path: Option<String>) {
    match CURRENT_LOG_STREAM_SOCKET_PATH.lock() {
        Ok(mut g) => *g = path,
        Err(e) => *e.into_inner() = path,
    }
}

/// C: src/log.c:390 current_log_stream_socket_path (read by src/x.c:1544).
pub fn current_log_stream_socket_path() -> Option<String> {
    CURRENT_LOG_STREAM_SOCKET_PATH.lock().ok().and_then(|g| g.clone())
}

/// C: src/main.c:188-190 (i3_exit) and src/commands.c:1715-1716 (cmd_restart)
/// — "if (current_log_stream_socket_path != NULL) unlink(...)".
///
/// A restart exec()s a new i3, which picks its own
/// /tmp/i3-*/log-stream-socket.<newpid>, so the old file has to go here or it
/// stays behind forever.
pub fn unlink_log_stream_socket() {
    if let Some(path) = current_log_stream_socket_path() {
        let _ = std::fs::remove_file(&path);
    }
}

/// C: src/log.c log_client — one accepted log-stream client.
pub struct LogClient {
    /// C: int fd
    pub fd: i32,
}

/// C: src/log.c — the "static TAILQ_HEAD(log_clients) log_clients" list of the
/// clients i3-dump-log -f opened. C keeps it as a global; the port keeps it
/// behind a Mutex because vlog() runs on the event-loop thread only.
static LOG_CLIENTS: Mutex<Vec<LogClient>> = Mutex::new(Vec::new());

/// C: src/log.c:399 void log_new_client(EV_P_ ev_io *w, int revents)
///
/// Accepts one connection on the log-stream listening socket and appends it to
/// the client list. Returns the new fd (None when nothing was pending).
pub fn log_new_client(listen_fd: i32) -> Option<i32> {
    let mut peer: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let fd = unsafe {
        libc::accept(
            listen_fd,
            &mut peer as *mut libc::sockaddr_un as *mut libc::sockaddr,
            &mut len,
        )
    };
    if fd < 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        // C: if (errno != EINTR) perror("accept()");
        //
        // C is only ever called from the ev_io watcher, i.e. with a pending
        // connection, so accept() cannot report EAGAIN there. The port calls
        // this once per loop iteration (the same shape as the IPC listener),
        // so EAGAIN simply means that no new client arrived.
        if errno != libc::EINTR && errno != libc::EAGAIN && errno != libc::EWOULDBLOCK {
            libi3::err::perror("accept()");
        }
        return None;
    }

    /* Close this file descriptor on exec() */
    // C: (void)fcntl(fd, F_SETFD, FD_CLOEXEC);
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    // C: set_nonblock(fd);
    libi3::nonblock::set_nonblock(fd);

    // C: client = scalloc(1, sizeof(log_client)); client->fd = fd;
    //    TAILQ_INSERT_TAIL(&log_clients, client, clients);
    match LOG_CLIENTS.lock() {
        Ok(mut clients) => clients.push(LogClient { fd }),
        Err(e) => e.into_inner().push(LogClient { fd }),
    }

    // C: DLOG("log: new client connected on fd %d\n", fd);
    debuglog(&format!("log: new client connected on fd {}\n", fd));
    Some(fd)
}

/// C: the log_clients list — the fds the main loop polls (C has one ev_io
/// watcher per client; reads from a client are ignored).
pub fn log_client_fds() -> Vec<i32> {
    match LOG_CLIENTS.lock() {
        Ok(clients) => clients.iter().map(|c| c.fd).collect(),
        Err(e) => e.into_inner().iter().map(|c| c.fd).collect(),
    }
}

/// C: src/log.c:422 void log_broadcast_to_clients(const char *message, size_t len)
///
/// Writes the log line to every connected client and drops the ones whose
/// write failed. Returns the number of clients that were written to.
pub fn log_broadcast_to_clients(message: &str) -> usize {
    let bytes = message.as_bytes();
    let mut clients = match LOG_CLIENTS.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let mut delivered = 0;
    let mut i = 0;
    while i < clients.len() {
        // C: ssize_t n = writeall(current->fd, message, len);
        //
        // C: libi3/safewrappers.c:73 — writeall() retries on EAGAIN, so a
        // client that stops reading blocks the window manager here, exactly
        // like in C (i3-dump-log is the only consumer).
        let n = libi3::safewrappers::writeall(clients[i].fd, bytes);
        if n < 0 {
            // C: TAILQ_REMOVE(&log_clients, previous, clients); free(previous);
            // C does not close(fd), and the port keeps that as well.
            clients.remove(i);
            continue;
        }
        delivered += 1;
        i += 1;
    }
    delivered
}

/// The port counterpart of C having no read watcher on a log client
/// (src/log.c:399-420 only ever writes): the main loop polls the client fds, so
/// the bytes they sent are read and discarded here and a client that closed its
/// end is dropped, otherwise the polled fd would report POLLIN forever.
pub fn service_log_clients() -> usize {
    let mut clients = match LOG_CLIENTS.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let mut buf = [0u8; 4096];
    let mut i = 0;
    while i < clients.len() {
        let fd = clients[i].fd;
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if n < 0 && (errno == libc::EAGAIN || errno == libc::EWOULDBLOCK || errno == libc::EINTR) {
            i += 1;
            continue;
        }
        if n <= 0 {
            // A failed read, or i3-dump-log closed its end: the next broadcast
            // would fail anyway, so drop the client now.
            unsafe {
                libc::close(fd);
            }
            clients.remove(i);
            continue;
        }
        // input from a log client is ignored (C never reads it)
    }
    clients.len()
}

/// Test-only: forget every client without writing to it (the C list is empty
/// at start-up and never reset; the tests share this process).
#[cfg(test)]
fn reset_log_clients() {
    if let Ok(mut clients) = LOG_CLIENTS.lock() {
        for c in clients.drain(..) {
            unsafe {
                libc::close(c.fd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log globals (shmlog_size, the SHM ring, the verbosity switches) are
    /// process-wide, exactly like the C statics. Cargo runs the tests of one
    /// binary in parallel, so the log tests must not race each other.
    static LOG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn default_shmlog_size_is_25_mib() {
        let _guard = lock();
        assert_eq!(default_shmlog_size(), 25 * 1024 * 1024);
    }

    #[test]
    fn verbosity_and_debug_toggles() {
        let _guard = lock();
        set_verbosity(true);
        assert!(get_verbosity());
        set_verbosity(false);
        assert!(!get_verbosity());
        set_debug_logging(true);
        assert!(get_debug_logging());
        set_debug_logging(false);
        assert!(!get_debug_logging());
    }

    #[test]
    fn header_layout_matches_shmlog_h() {
        let _guard = lock();
        // C: include/shmlog.h — 5 uint32_t fields, 20 bytes.
        assert_eq!(std::mem::size_of::<i3_shmlog_header>(), 20);
    }

    #[test]
    fn time_string_has_the_c_prefix_shape() {
        let _guard = lock();
        // C: strftime("%x %X - ") — locale dependent, always ends with " - ".
        let s = current_time_string();
        assert!(s.ends_with(" - "), "prefix was {:?}", s);
        assert!(s.len() > 4);
    }

    #[test]
    fn purge_zerobyte_logfile_removes_only_empty_logs() {
        let _guard = lock();
        // C: src/log.c:365-388 — an empty error log is unlinked and its
        // directory removed; a log with content is kept.
        let base = std::env::temp_dir().join(format!("i3-purge-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let empty_dir = base.join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let empty = empty_dir.join("errorlog.1");
        std::fs::write(&empty, b"").unwrap();
        set_errorfilename(Some(empty.to_string_lossy().into_owned()));
        purge_zerobyte_logfile();
        assert!(!empty.exists(), "an empty errorlog is deleted");
        assert!(!empty_dir.exists(), "the directory of an empty errorlog is deleted");

        let full_dir = base.join("full");
        std::fs::create_dir_all(&full_dir).unwrap();
        let full = full_dir.join("errorlog.2");
        std::fs::write(&full, b"ERROR: something
").unwrap();
        set_errorfilename(Some(full.to_string_lossy().into_owned()));
        purge_zerobyte_logfile();
        assert!(full.exists(), "a non-empty errorlog is kept");
        assert!(full_dir.exists());

        set_errorfilename(None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// C: src/log.c:172 mmap(MAP_SHARED) + i3-dump-log/main.c:193 — the ring
    /// buffer has to be visible through a second mapping of the segment,
    /// otherwise i3-dump-log reads an empty log (207-shmlog.t).
    #[test]
    fn ring_buffer_is_the_shared_segment() {
        use std::ffi::CString;

        let _guard = lock();
        // Other tests log through con_focus()/DLOG while this one runs, so the
        // ring must be large enough not to wrap around the marker before it is
        // read back (the C ring is 25 MiB in a debug build for the same reason).
        const SIZE: usize = 1 << 20;
        set_shmlog_size(SIZE);
        init_logging();
        assert!(logging_enabled());

        set_debug_logging(true);
        debuglog("shared-segment-marker\n");
        set_debug_logging(false);

        let name = shmlog_name().expect("the segment name is exported");
        let cname = CString::new(name).unwrap();
        let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0) };
        assert!(fd >= 0, "i3-dump-log can shm_open the segment");
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SIZE,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert!(ptr != libc::MAP_FAILED);
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, SIZE) };
        let text = String::from_utf8_lossy(bytes);
        assert!(
            text.contains("shared-segment-marker"),
            "the mapping must show what vlog wrote: {:?}",
            &text[..200.min(text.len())]
        );
        unsafe {
            libc::munmap(ptr, SIZE);
            libc::close(fd);
        }

        set_shmlog_size(0);
        init_logging();
        assert!(!logging_enabled(), "shmlog off closes the segment");
        assert!(shmlog_name().is_none());
    }

    #[test]
    fn ring_buffer_wraps_and_counts() {
        let _guard = lock();
        set_shmlog_size(1024);
        init_logging();
        assert!(logging_enabled());
        // The ring buffer is only fed by vlog(), which debuglog() reaches
        // only while debug logging is enabled (C: src/log.c DLOG).
        set_debug_logging(true);
        // Write more than the buffer size to force a wrap.
        for _ in 0..100 {
            debuglog("x");
        }
        set_debug_logging(false);
        if let Ok(g) = logbuffer_cell().lock() {
            let buf = g.as_ref().unwrap();
            let hdr = read_header(buf);
            assert_eq!(hdr.size as usize, buf.len());
            assert!(hdr.wrap_count > 0, "wrapping must be counted");
        }
        set_shmlog_size(0);
        init_logging();
        assert!(!logging_enabled());
    }

    /// C: src/log.c:399 log_new_client() + src/log.c:422
    /// log_broadcast_to_clients() — a client that connected through the
    /// listening socket receives every log line, and the client whose write
    /// fails is removed from the list.
    #[test]
    fn log_stream_broadcast_delivers_and_drops_failed_clients() {
        use std::io::Read as _;
        use std::os::unix::net::UnixStream;

        let _guard = lock();
        reset_log_clients();

        let dir = std::env::temp_dir().join(format!("i3-logstream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("log-stream-socket");
        let mut bound = None;
        // C: libi3/create_socket.c — bind() + listen() + set_nonblock().
        let listen_fd = libi3::create_socket(&path.to_string_lossy(), &mut bound);
        assert!(listen_fd >= 0, "create_socket failed");
        assert_eq!(bound, Some(path.to_string_lossy().into_owned()));
        assert!(path.exists(), "the socket file exists while i3 listens");

        let client = UnixStream::connect(&path).unwrap();
        // C: log_new_client() accept()s the pending connection.
        let accepted = log_new_client(listen_fd).expect("the pending client is accepted");
        assert_eq!(log_client_fds(), vec![accepted]);
        // C: fcntl(..., FD_CLOEXEC) + set_nonblock() on the accepted fd.
        let flags = unsafe { libc::fcntl(accepted, libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "FD_CLOEXEC is set");
        let flags = unsafe { libc::fcntl(accepted, libc::F_GETFL) };
        assert_ne!(flags & libc::O_NONBLOCK, 0, "the client fd is non-blocking");

        // C: log_broadcast_to_clients(message, len) — one writeall() per client.
        assert_eq!(log_broadcast_to_clients("first line\n"), 1);
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 64];
        let mut reader = &client;
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"first line\n");

        // C: src/log.c:469 (in vlog()) log_broadcast_to_clients(message, len) —
        // the path every LOG() line takes, i.e. what i3-msg triggers.
        set_shmlog_size(1 << 16);
        init_logging();
        log_("via vlog\n");
        set_shmlog_size(0);
        init_logging();
        let n = reader.read(&mut buf).unwrap();
        let line = String::from_utf8_lossy(&buf[..n]).into_owned();
        assert!(line.ends_with("via vlog\n"), "vlog streamed {:?}", line);

        // C: the client list is write-only; the port drains it so poll() does
        // not spin. A client that closed its end is dropped here.
        assert_eq!(service_log_clients(), 1, "a quiet client stays connected");
        drop(client);
        assert_eq!(service_log_clients(), 0, "a closed client is removed");
        assert!(log_client_fds().is_empty());

        // C: writeall() failing (EPIPE) removes the client from the list.
        let client = UnixStream::connect(&path).unwrap();
        let accepted = log_new_client(listen_fd).expect("second client accepted");
        assert_eq!(log_client_fds(), vec![accepted]);
        drop(client);
        assert_eq!(log_broadcast_to_clients("nobody listens\n"), 0);
        assert!(log_client_fds().is_empty(), "the failed client was removed");

        reset_log_clients();
        unsafe {
            libc::close(listen_fd);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C: libi3/get_process_filename.c:61 — "<dir>/log-stream-socket.<pid>",
    /// handed to create_socket() by src/main.c:1016-1017, exported by
    /// src/x.c:1544 and unlinked again by src/main.c:188.
    #[test]
    fn log_stream_socket_path_is_per_process() {
        use std::os::unix::fs::FileTypeExt as _;

        let _guard = lock();
        reset_log_clients();

        let path = libi3::get_process_filename("log-stream-socket").expect("a /tmp path");
        assert!(
            path.ends_with(&format!("/log-stream-socket.{}", std::process::id())),
            "path was {:?}",
            path
        );

        let mut bound = None;
        let listen_fd = libi3::create_socket(&path, &mut bound);
        assert!(listen_fd >= 0, "create_socket failed");
        // C: src/main.c:1017 current_log_stream_socket_path gets the path
        // create_socket() bound.
        set_current_log_stream_socket_path(bound);
        assert_eq!(current_log_stream_socket_path(), Some(path.clone()));
        assert!(std::fs::metadata(&path).unwrap().file_type().is_socket());

        // C: src/main.c:188-190 unlink(current_log_stream_socket_path).
        unlink_log_stream_socket();
        assert!(
            !std::path::Path::new(&path).exists(),
            "i3_exit removes the socket file"
        );

        set_current_log_stream_socket_path(None);
        assert_eq!(current_log_stream_socket_path(), None);
        unsafe {
            libc::close(listen_fd);
        }
    }
}