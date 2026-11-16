//! C: i3-dump-log/main.c (274 lines) — Dumps the i3 SHM log to stdout.
//!
//! The ring buffer is the on-disk ABI defined by include/shmlog.h: the first
//! sizeof(i3_shmlog_header) bytes of the SHM segment are the header, the rest
//! is the log text that vlog() (src/log.c) appends to and wraps around.
//! i3_core::log::i3_shmlog_header is reused verbatim so both sides agree on
//! the layout; this module only implements the reader half.
//!
//! Differences that are forced by Rust: C keeps raw pointers (logbuffer, walk)
//! and uses pointer arithmetic, the port keeps "walk" as an offset into the
//! mapped segment. Every observable behaviour (byte order of the output, the
//! "skip the first, mangled line" rule, the wrap counter handling) is kept.

#![allow(non_camel_case_types)]

use std::ffi::{CString, OsString};
use std::io::Write;

use i3_core::log::i3_shmlog_header;
use libi3::err::{err, errx};
use libi3::getopt::{getopt_long, option, Getopt};
use libi3::ipc::{I3_IPC_MESSAGE_TYPE_COMMAND, I3_IPC_REPLY_TYPE_COMMAND};
use libi3::safewrappers::swrite;
use libi3::{
    i3_version, ipc_connect, ipc_recv_message, ipc_send_message, is_debug_build,
    root_atom_contents,
};

/// C: include/shmlog.h sizeof(i3_shmlog_header) — the header sits at the start
/// of the segment and the log text starts right after it.
pub const HEADER_SIZE: usize = std::mem::size_of::<i3_shmlog_header>();

/// C: i3-dump-log/main.c:34 static i3_shmlog_header *header — the header is
/// read out of the mapping for every use (the writer updates it concurrently),
/// just like C dereferences the pointer each time.
pub fn read_header(segment: &[u8]) -> i3_shmlog_header {
    let mut header = i3_shmlog_header::default();
    let n = HEADER_SIZE.min(segment.len());
    unsafe {
        std::ptr::copy_nonoverlapping(segment.as_ptr(), &mut header as *mut _ as *mut u8, n);
    }
    header
}

/// C: i3-dump-log/main.c:32 static uint32_t wrap_count — the wrap counter the
/// dumper has already seen, plus C's "walk" pointer (kept as an offset).
pub struct dump_ctx {
    /// C: i3-dump-log/main.c:32 wrap_count
    pub wrap_count: u32,
    /// C: i3-dump-log/main.c:35-36 char *walk
    pub walk: usize,
}

impl Default for dump_ctx {
    fn default() -> Self {
        dump_ctx {
            wrap_count: 0,
            walk: 0,
        }
    }
}

impl dump_ctx {
    /// C: i3-dump-log/main.c:56 static int check_for_wrap(void)
    ///
    /// Returns 1 when the log wrapped (and the remaining content was printed),
    /// 0 otherwise.
    pub fn check_for_wrap(
        &mut self,
        segment: &[u8],
        header: &i3_shmlog_header,
        out: &mut dyn Write,
    ) -> i32 {
        if self.wrap_count == header.wrap_count {
            return 0;
        }

        // C: The log wrapped. Print the remaining content and reset walk to the
        // top of the log.
        self.wrap_count = header.wrap_count;
        // C: const int len = (logbuffer + header->offset_last_wrap) - walk;
        let len = header.offset_last_wrap as isize - self.walk as isize;
        // C passes len to swrite() as a size_t; a negative length (only
        // possible with a corrupted header) is not turned into a write here.
        if len > 0 {
            let start = self.walk.min(segment.len());
            let end = (start + len as usize).min(segment.len());
            // C: swrite(STDOUT_FILENO, walk, len) — the port writes to the
            // writer handed in (main passes stdout), see run_dump().
            let _ = out.write_all(&segment[start..end]);
        }
        self.walk = HEADER_SIZE;
        1
    }

    /// C: i3-dump-log/main.c:70 static void print_till_end(void)
    pub fn print_till_end(
        &mut self,
        segment: &[u8],
        header: &i3_shmlog_header,
        out: &mut dyn Write,
    ) {
        self.check_for_wrap(segment, header, out);
        // C: const int len = (logbuffer + header->offset_next_write) - walk;
        let len = header.offset_next_write as isize - self.walk as isize;
        if len > 0 {
            let start = self.walk.min(segment.len());
            let end = (start + len as usize).min(segment.len());
            let _ = out.write_all(&segment[start..end]);
            self.walk += len as usize;
        }
    }
}

/// C: i3-dump-log/main.c:213-230 — the "print the old content, then the new
/// lines" sequence: skip the first (very likely mangled) line of the previous
/// cycle, print the remainder if the log wrapped, then dump everything from
/// the top of the ring buffer up to the write position.
pub fn dump_existing(segment: &[u8], out: &mut dyn Write) -> dump_ctx {
    let header = read_header(segment);
    let mut ctx = dump_ctx::default();

    // C: walk = logbuffer + header->offset_next_write;
    ctx.walk = header.offset_next_write as usize;

    // C: if (*walk != '\0') { while (*walk != '\n') walk++; walk++; }
    if ctx.walk < segment.len() && segment[ctx.walk] != 0 {
        while ctx.walk < segment.len() && segment[ctx.walk] != b'\n' {
            ctx.walk += 1;
        }
        // C: walk++ — past the '\n' (clamped at the end of the mapping).
        ctx.walk = (ctx.walk + 1).min(segment.len());
    }

    // C: wrap_count = 0; check_for_wrap();
    ctx.wrap_count = 0;
    ctx.check_for_wrap(segment, &header, out);

    // C: walk = logbuffer + sizeof(i3_shmlog_header); print_till_end();
    ctx.walk = HEADER_SIZE;
    // print_till_end() calls check_for_wrap() again; the counter is unchanged
    // so that call is a no-op, exactly like in C.
    ctx.print_till_end(segment, &header, out);
    let _ = out.flush();
    ctx
}

/// C: i3-dump-log/main.c:39 static void disable_shmlog(void) — the atexit
/// handler that turns SHM logging off again when this dumper enabled it.
pub fn disable_shmlog(ipcfd: i32) {
    let disablecmd = "debuglog off; shmlog off";
    if ipc_send_message(
        ipcfd,
        disablecmd.len() as u32,
        I3_IPC_MESSAGE_TYPE_COMMAND,
        disablecmd.as_bytes(),
    ) != 0
    {
        err(1, "IPC send");
    }

    // C: Ensure the command was sent by waiting for the reply.
    let mut reply_length: u32 = 0;
    let mut reply: Vec<u8> = Vec::new();
    let mut message_type = I3_IPC_REPLY_TYPE_COMMAND;
    if ipc_recv_message(ipcfd, Some(&mut message_type), &mut reply_length, &mut reply) != 0 {
        err(1, "IPC recv");
    }
}

/// Runs the atexit handler when the process finishes the way C's atexit() does
/// (the Rust port expresses atexit(disable_shmlog) as a Drop guard because
/// main() returns an exit code instead of calling exit()).
struct AtexitDisableShmlog {
    ipcfd: i32,
}
impl Drop for AtexitDisableShmlog {
    fn drop(&mut self) {
        disable_shmlog(self.ipcfd);
    }
}

/// A read-only mapping of the SHM log segment (C: shm_open + fstat + mmap).
pub struct ShmSegment {
    fd: i32,
    ptr: *const u8,
    len: usize,
    name: String,
}

/// Which C step failed, so run() can print the exact err() message.
#[derive(Debug)]
pub enum ShmError {
    /// C: shm_open() — carries errno.
    Open(i32),
    /// C: fstat()
    Fstat(i32),
    /// C: mmap()
    Mmap(i32),
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

impl ShmSegment {
    /// C: i3-dump-log/main.c:184-199 — shm_open(O_RDWR) + fstat + mmap(PROT_READ).
    ///
    /// NB (C comment): While we must never write, we need O_RDWR for the
    /// pthread condvar.
    pub fn open(shmname: &str) -> Result<ShmSegment, ShmError> {
        let cname = match CString::new(shmname) {
            Ok(c) => c,
            Err(_) => return Err(ShmError::Open(libc::EINVAL)),
        };
        let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0) };
        if fd == -1 {
            return Err(ShmError::Open(errno()));
        }

        // C: struct stat statbuf; fstat(logbuffer_shm, &statbuf)
        let mut statbuf: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut statbuf) } != 0 {
            let e = errno();
            unsafe { libc::close(fd) };
            return Err(ShmError::Fstat(e));
        }
        let len = statbuf.st_size as usize;

        // C: logbuffer = mmap(NULL, statbuf.st_size, PROT_READ, MAP_SHARED, ...)
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            let e = errno();
            unsafe { libc::close(fd) };
            return Err(ShmError::Mmap(e));
        }

        Ok(ShmSegment {
            fd,
            ptr: ptr as *const u8,
            len,
            name: shmname.to_string(),
        })
    }

    /// C: the logbuffer pointer as a slice of the mapping.
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
            libc::close(self.fd);
        }
    }
}

/// What the command line asks for (C: the "verbose" and "follow" flags).
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Options {
    pub verbose: bool,
    pub follow: bool,
}

/// The result of parsing argv (C: the -v / -h early returns).
#[derive(Debug, PartialEq, Eq)]
pub enum ParseOutcome {
    Version,
    Help,
    Run(Options),
}

/// C: i3-dump-log/main.c:85-126 — the getopt_long loop.
pub fn parse_args(argv: Vec<OsString>) -> ParseOutcome {
    let mut go = Getopt::new(argv);
    let long_options = vec![
        option { name: "version", has_arg: libi3::getopt::no_argument, val: b'v' },
        option { name: "verbose", has_arg: libi3::getopt::no_argument, val: b'V' },
        option { name: "follow", has_arg: libi3::getopt::no_argument, val: b'f' },
        option { name: "help", has_arg: libi3::getopt::no_argument, val: b'h' },
    ];
    // C: char *options_string = "s:vfVh";
    let options_string = "s:vfVh";

    let mut opts = Options::default();
    loop {
        let o = getopt_long(&mut go, options_string, &long_options);
        if o == -1 {
            break;
        }
        if o == b'v' as i32 {
            return ParseOutcome::Version;
        } else if o == b'V' as i32 {
            opts.verbose = true;
        } else if o == b'f' as i32 {
            opts.follow = true;
        } else if o == b'h' as i32 {
            return ParseOutcome::Help;
        }
        // 's' is accepted by the option string but never looked at (the C
        // switch has no case for it either) and '?' is ignored — 1:1.
    }
    ParseOutcome::Run(opts)
}

/// C: i3-dump-log/main.c:109 printf("i3-dump-log " I3_VERSION "\n")
pub fn print_version() {
    print!("i3-dump-log {}\n", i3_version());
}

/// C: i3-dump-log/main.c:118-124
pub fn print_help() {
    print_version();
    print!("i3-dump-log [-fhVv]\n");
}

/// C: i3-dump-log/main.c:85 int main(int argc, char *argv[]) — split out of
/// main() so the CLI can be exercised in-process (main just calls this).
pub fn run_with_args(argv: Vec<OsString>) -> i32 {
    match parse_args(argv) {
        ParseOutcome::Version => {
            print_version();
            0
        }
        ParseOutcome::Help => {
            print_help();
            0
        }
        ParseOutcome::Run(opts) => run_dump(opts),
    }
}

/// C: i3-dump-log/main.c:128-272 — everything after option parsing.
fn run_dump(opts: Options) -> i32 {
    let mut shmname = root_atom_contents("I3_SHMLOG_PATH", None, 0);
    let mut _atexit_guard: Option<AtexitDisableShmlog> = None;

    if shmname.is_none() {
        // C: Something failed. Let's invest a little effort to find out what
        // it is. This is hugely helpful for users who want to debug i3 but are
        // not used to the procedure yet.
        let conn = x11rb_connect();
        let (conn, screen) = match conn {
            Some(c) => c,
            None => {
                eprint!("i3-dump-log: ERROR: Cannot connect to X11.\n\n");
                match std::env::var("DISPLAY") {
                    Err(_) => {
                        eprint!("Your DISPLAY environment variable is not set.\n");
                        eprint!("Are you running i3-dump-log via SSH or on a virtual console?\n");
                        eprint!("Try DISPLAY=:0 i3-dump-log\n");
                        return 1;
                    }
                    Ok(display) => {
                        eprint!(
                            "FYI: The DISPLAY environment variable is set to \"{}\".\n",
                            display
                        );
                        return 1;
                    }
                }
            }
        };
        if root_atom_contents("I3_CONFIG_PATH", Some(&conn), screen as i32).is_some() {
            eprint!("i3-dump-log: i3 is running, but SHM logging is not enabled. Enabling SHM log now while i3-dump-log is running\n\n");
            let ipcfd = ipc_connect(None);
            let enablecmd = "debuglog on; shmlog 5242880";
            if ipc_send_message(
                ipcfd,
                enablecmd.len() as u32,
                I3_IPC_MESSAGE_TYPE_COMMAND,
                enablecmd.as_bytes(),
            ) != 0
            {
                err(1, "IPC send");
            }
            // C: By the time we receive a reply, I3_SHMLOG_PATH is set.
            let mut reply_length: u32 = 0;
            let mut reply: Vec<u8> = Vec::new();
            let mut message_type = I3_IPC_REPLY_TYPE_COMMAND;
            if ipc_recv_message(ipcfd, Some(&mut message_type), &mut reply_length, &mut reply) != 0
            {
                err(1, "IPC recv");
            }

            _atexit_guard = Some(AtexitDisableShmlog { ipcfd });

            // C: Retry:
            shmname = root_atom_contents("I3_SHMLOG_PATH", None, 0);
            if shmname.is_none() && !is_debug_build() {
                eprint!(
                    "You seem to be using a release version of i3:\n  {}\n\n",
                    i3_version()
                );
                eprint!("Release versions do not use SHM logging by default,\ntherefore i3-dump-log does not work.\n\n");
                eprint!("Please follow this guide instead:\nhttps://i3wm.org/docs/debugging-release-version.html\n");
                return 1;
            }
        }
        if shmname.is_none() {
            errx(1, "Cannot get I3_SHMLOG_PATH atom contents. Is i3 running on this display?");
        }
    }

    let shmname = shmname.unwrap_or_default();
    if shmname.is_empty() {
        errx(1, "Cannot dump log: SHM logging is disabled in i3.");
    }

    // C: i3-dump-log/main.c:187-199
    let segment = match ShmSegment::open(&shmname) {
        Ok(s) => s,
        Err(ShmError::Open(_)) => err(
            1,
            &format!("Could not shm_open SHM segment for the i3 log ({})", shmname),
        ),
        Err(ShmError::Fstat(_)) => err(1, &format!("stat({})", shmname)),
        Err(ShmError::Mmap(_)) => err(1, "Could not mmap SHM segment for the i3 log"),
    };

    let slice = segment.as_slice();

    // C: header = (i3_shmlog_header *)logbuffer;
    let header = read_header(slice);

    if opts.verbose {
        // C: i3-dump-log/main.c:204-205
        print!(
            "next_write = {}, last_wrap = {}, logbuffer_size = {}, shmname = {}\n",
            header.offset_next_write, header.offset_last_wrap, header.size, shmname
        );
    }

    let mut out = std::io::stdout();
    dump_existing(slice, &mut out);

    if !opts.follow {
        return 0;
    }

    // C: i3-dump-log/main.c:237-269 — the log stream socket.
    let log_stream_socket_path = match root_atom_contents("I3_LOG_STREAM_SOCKET_PATH", None, 0) {
        Some(p) => p,
        None => errx(
            1,
            "could not determine i3 log stream socket path: possible i3-dump-log and i3 version mismatch",
        ),
    };

    let sockfd = unsafe { libc::socket(libc::AF_LOCAL, libc::SOCK_STREAM, 0) };
    if sockfd == -1 {
        err(1, "Could not create socket");
    }
    // C: (void)fcntl(sockfd, F_SETFD, FD_CLOEXEC);
    unsafe {
        libc::fcntl(sockfd, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_LOCAL as libc::sa_family_t;
    // C: strncpy(addr.sun_path, log_stream_socket_path, sizeof(addr.sun_path) - 1);
    let bytes = log_stream_socket_path.as_bytes();
    let len = bytes.len().min(addr.sun_path.len() - 1);
    for (d, s) in addr.sun_path[..len].iter_mut().zip(bytes) {
        *d = *s as libc::c_char;
    }
    if unsafe {
        libc::connect(
            sockfd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    } < 0
    {
        err(
            1,
            &format!("Could not connect to i3 on socket {}", log_stream_socket_path),
        );
    }

    // C: Same size as the buffer used in log.c vlog(): char buf[4096];
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe {
            libc::read(
                sockfd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len() as libc::size_t,
            )
        };
        if n == -1 {
            err(1, "read(log-stream-socket):");
        }
        if n == 0 {
            // C: exit(0) — i3 closed the socket.
            unsafe { libc::close(sockfd) };
            return 0;
        }
        swrite(1, &buf[..n as usize]);
    }
}

/// C: i3-dump-log/main.c:133-136 — xcb_connect(NULL, &screen) plus the
/// xcb_connection_has_error() check, mapped onto x11rb's connection type.
fn x11rb_connect() -> Option<(x11rb::rust_connection::RustConnection, usize)> {
    // x11rb's RustConnection::connect() already fails for an unreachable
    // display, which covers xcb_connection_has_error() as well.
    x11rb::rust_connection::RustConnection::connect(None).ok()
}

/// C: i3-dump-log/main.c:85 int main(int argc, char *argv[])
pub fn run() -> i32 {
    run_with_args(std::env::args_os().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The writer half of the ring buffer, mirroring src/log.c:vlog()'s
    /// ring-buffer code so that the reader can be tested as a real
    /// write-then-read round trip.
    struct RingWriter {
        map: Vec<u8>,
        walk: usize,
        wrap_count: u32,
        last_wrap: usize,
    }

    impl RingWriter {
        fn new(size: usize) -> RingWriter {
            let mut w = RingWriter {
                map: vec![0u8; size],
                walk: HEADER_SIZE,
                wrap_count: 0,
                last_wrap: size,
            };
            w.write_header();
            w
        }

        fn write_header(&mut self) {
            let header = i3_shmlog_header {
                offset_next_write: self.walk as u32,
                offset_last_wrap: self.last_wrap as u32,
                size: self.map.len() as u32,
                wrap_count: self.wrap_count,
                wrap_count_last: 0,
            };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    &header as *const i3_shmlog_header as *const u8,
                    HEADER_SIZE,
                )
            };
            self.map[..HEADER_SIZE].copy_from_slice(bytes);
        }

        /// C: src/log.c vlog() ring-buffer write.
        fn write(&mut self, msg: &str) {
            let bytes = msg.as_bytes();
            if self.walk + bytes.len() > self.last_wrap {
                self.walk = HEADER_SIZE;
                self.wrap_count += 1;
            }
            let end = (self.walk + bytes.len()).min(self.map.len());
            let n = end - self.walk;
            self.map[self.walk..end].copy_from_slice(&bytes[..n]);
            self.walk = end;
            self.write_header();
        }

        fn segment(&self) -> &[u8] {
            &self.map
        }
    }

    fn dump_to_string(segment: &[u8]) -> String {
        let mut out: Vec<u8> = Vec::new();
        dump_existing(segment, &mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn header_size_matches_shmlog_h() {
        // C: include/shmlog.h — 5 uint32_t, 20 bytes.
        assert_eq!(HEADER_SIZE, 20);
    }

    #[test]
    fn empty_log_prints_nothing() {
        let w = RingWriter::new(1024);
        assert_eq!(dump_to_string(w.segment()), "");
    }

    #[test]
    fn roundtrip_without_wrap_prints_records_in_order() {
        let mut w = RingWriter::new(1024);
        w.write("one\n");
        w.write("two\n");
        w.write("three\n");
        assert_eq!(dump_to_string(w.segment()), "one\ntwo\nthree\n");
    }

    #[test]
    fn roundtrip_with_wrap_prints_old_then_new() {
        // 64 bytes total: 20 header + 44 log. "lineN\n" is 6 bytes, so the
        // eighth record does not fit any more and wraps the buffer.
        let mut w = RingWriter::new(64);
        for i in 0..8 {
            w.write(&format!("line{}\n", i));
        }
        assert_eq!(w.wrap_count, 1);
        // Old content first (the mangled first line is skipped, the two
        // untouched bytes at the end of the previous cycle are printed like C
        // does), then the newest record.
        assert_eq!(
            dump_to_string(w.segment()),
            "line2\nline3\nline4\nline5\nline6\n\0\0line7\n"
        );
    }

    #[test]
    fn wrapped_log_skips_the_mangled_first_line() {
        // A record that fills the ring buffer to its very end leaves a tail
        // that starts in the middle of a line: exactly the case C skips.
        let mut w = RingWriter::new(64);
        let fill = format!("first\nsecond\nthird\n{}", "p".repeat(25));
        assert_eq!(fill.len(), 44);
        w.write(&fill);
        w.write("NEW\n");
        assert_eq!(w.wrap_count, 1);
        // "first\n" is the mangled line and is dropped; the rest of the old
        // cycle is printed, then the new record.
        assert_eq!(
            dump_to_string(w.segment()),
            format!("second\nthird\n{}NEW\n", "p".repeat(25))
        );
    }

    #[test]
    fn check_for_wrap_only_fires_once_per_wrap() {
        let mut w = RingWriter::new(64);
        w.write("a\n");
        let header = read_header(w.segment());
        let mut ctx = dump_ctx {
            wrap_count: header.wrap_count,
            walk: HEADER_SIZE,
        };
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(ctx.check_for_wrap(w.segment(), &header, &mut out), 0);
        let header2 = i3_shmlog_header { wrap_count: 7, ..header };
        assert_eq!(ctx.check_for_wrap(w.segment(), &header2, &mut out), 1);
        assert_eq!(ctx.wrap_count, 7);
        assert_eq!(ctx.walk, HEADER_SIZE);
        // Same header again: nothing to do.
        assert_eq!(ctx.check_for_wrap(w.segment(), &header2, &mut out), 0);
    }

    #[test]
    fn real_shm_segment_roundtrip() {
        // The real mapping path: shm_open + ftruncate + mmap, write records
        // like i3 does, then dump through ShmSegment like i3-dump-log does.
        let name = format!("/i3-dump-log-test-{}", std::process::id());
        let cname = CString::new(name.clone()).unwrap();
        unsafe {
            libc::shm_unlink(cname.as_ptr());
        }
        let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o600) };
        assert!(fd >= 0, "shm_open failed");
        let size = 4096usize;
        assert_eq!(unsafe { libc::ftruncate(fd, size as libc::off_t) }, 0);

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(ptr, libc::MAP_FAILED);
        {
            let map = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, size) };
            for b in map.iter_mut() {
                *b = 0;
            }
            let mut walk = HEADER_SIZE;
            for msg in ["hello\n", "world\n"] {
                map[walk..walk + msg.len()].copy_from_slice(msg.as_bytes());
                walk += msg.len();
            }
            let header = i3_shmlog_header {
                offset_next_write: walk as u32,
                offset_last_wrap: size as u32,
                size: size as u32,
                wrap_count: 0,
                wrap_count_last: 0,
            };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    &header as *const i3_shmlog_header as *const u8,
                    HEADER_SIZE,
                )
            };
            map[..HEADER_SIZE].copy_from_slice(bytes);
        }
        unsafe {
            libc::munmap(ptr, size);
        }
        unsafe {
            libc::close(fd);
        }

        let segment = ShmSegment::open(&name).expect("ShmSegment::open");
        assert_eq!(segment.len(), size);
        let header = read_header(segment.as_slice());
        assert_eq!(header.size as usize, size);
        assert_eq!(dump_to_string(segment.as_slice()), "hello\nworld\n");
        drop(segment);

        unsafe {
            libc::shm_unlink(cname.as_ptr());
        }
    }

    #[test]
    fn parse_args_follow_and_verbose() {
        let argv: Vec<OsString> = ["i3-dump-log", "-f", "--verbose"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(
            parse_args(argv),
            ParseOutcome::Run(Options {
                verbose: true,
                follow: true
            })
        );
        let argv: Vec<OsString> = ["i3-dump-log", "-v"].iter().map(OsString::from).collect();
        assert_eq!(parse_args(argv), ParseOutcome::Version);
        let argv: Vec<OsString> = ["i3-dump-log", "--help"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(parse_args(argv), ParseOutcome::Help);
        // C: "s:" takes an argument but the option is never handled.
        let argv: Vec<OsString> = ["i3-dump-log", "-s", "/tmp/x"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(parse_args(argv), ParseOutcome::Run(Options::default()));
    }
}
