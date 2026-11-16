//! C: libc getopt_long(3) — shared by every i3 binary (i3, i3-msg, …).
//! Originally written for crates/bins/i3-msg; moved here so bins/i3 reuses it
//! exactly like the C build links the same libc.
//! i3-msg/main.c:171. libc itself is not part of the 1:1 port, so the
//! subset i3-msg relies on is implemented here with the same observable
//! behavior: short option clusters, long options with abbreviations,
//! glibc-style error messages (prefixed with argv[0] verbatim) and GNU
//! argv permutation (non-options are deferred, options may follow them).
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// C: <getopt.h> enum — has_arg values. i3-msg uses no_argument and
/// required_argument only (optional_argument is not represented).
pub const no_argument: i32 = 0;
pub const required_argument: i32 = 1;

/// C: struct option (getopt.h) — i3-msg always passes flag = 0, so the
/// flag member is not represented.
pub struct option {
    pub name: &'static str,
    pub has_arg: i32,
    /// C: int *flag — always NULL in i3-msg.
    pub val: u8,
}

/// getopt state — the C variables optind, optarg, optopt plus the
/// bookkeeping GNU getopt needs for its argv permutation.
pub struct Getopt {
    /// C: argv (argv[0] first; kept verbatim for getopt error messages).
    args: Vec<OsString>,
    /// C: optind — index of the next argv element to examine.
    optind: usize,
    /// C: nextchar — byte position within the current short option cluster.
    nextchar: Option<usize>,
    /// C: non-options collected while permuting (GNU exchange behavior).
    nonopts: Vec<OsString>,
    /// C: optarg — argument of the last matched option.
    pub optarg: Option<OsString>,
    /// C: optopt — the last unknown option character.
    pub optopt: u8,
    /// C: option_index — the index into longopts of the last matched long
    /// option; used by main.c to switch on long_options[option_index].name.
    pub option_index: usize,
    /// The name of the last matched long option (C: the pointer arithmetic
    /// `long_options[option_index].name` that main.c then strcmp()s).
    pub long_name: String,
}

impl Getopt {
    pub fn new(args: Vec<OsString>) -> Getopt {
        Getopt {
            args,
            optind: 1,
            nextchar: None,
            nonopts: Vec::new(),
            optarg: None,
            optopt: 0,
            option_index: 0,
            long_name: String::new(),
        }
    }

    /// C: argv[optind..argc] after getopt_long returned -1, with the
    /// non-options permuted in front of them.
    pub fn remaining(&self) -> Vec<OsString> {
        let mut rest: Vec<OsString> = self.nonopts.clone();
        rest.extend(self.args[self.optind.min(self.args.len())..].iter().cloned());
        rest
    }

    /// C: long_options[option_index].name — what main.c switches on in the
    /// `case 0:` branch.
    pub fn long_option_name(&self) -> String {
        self.long_name.clone()
    }

    /// C: optind — the index of the next argv element to examine. i3-nagbar and
    /// i3-config-wizard read argv[optind] directly (the button action) and bump
    /// optind themselves, so the libc globals need to be reachable.
    pub fn optind(&self) -> usize {
        self.optind
    }

    /// C: argv[i]
    pub fn argv(&self, i: usize) -> Option<OsString> {
        self.args.get(i).cloned()
    }

    /// C: optind = value
    pub fn set_optind(&mut self, value: usize) {
        self.optind = value;
    }

    /// C: argc
    pub fn argc(&self) -> usize {
        self.args.len()
    }

    fn progname(&self) -> String {
        self.args.first().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// C: getopt_long(argc, argv, optstring, long_options, &option_index) —
/// returns the option character, '?' on an (reported) error, or -1 when
/// the arguments are exhausted.
pub fn getopt_long(go: &mut Getopt, optstring: &str, longopts: &[option]) -> i32 {
    loop {
        // C: while ((o = getopt_long(...)) != -1)
        if go.optind >= go.args.len() {
            return -1;
        }
        let arg = go.args[go.optind].clone();
        let bytes = arg.as_bytes().to_vec();

        // C: "--" terminates option parsing.
        if bytes == b"--" {
            go.optind += 1;
            go.nextchar = None;
            return -1;
        }

        // "-" alone and anything not starting with '-' is a non-option.
        // GNU getopt permutes it behind the remaining options.
        if bytes.len() <= 1 || bytes[0] != b'-' {
            go.nonopts.push(arg);
            go.optind += 1;
            go.nextchar = None;
            continue;
        }

        if bytes[1] == b'-' {
            // C: long option handling in glibc getopt_long().
            let body = &bytes[2..];
            let (name, attached) = match body.iter().position(|&b| b == b'=') {
                Some(p) => (&body[..p], Some(body[p + 1..].to_vec())),
                None => (body, None),
            };
            let name_str = lossy(name);

            // glibc: an exact match wins over prefix abbreviations.
            let mut matches: Vec<&option> =
                longopts.iter().filter(|o| o.name == name_str).collect();
            if matches.is_empty() {
                matches = longopts.iter().filter(|o| o.name.starts_with(&name_str)).collect();
            }

            if matches.is_empty() {
                eprintln!("{}: unrecognized option '{}'", go.progname(), arg.to_string_lossy());
                go.optind += 1;
                go.nextchar = None;
                return b'?' as i32;
            }
            if matches.len() > 1 {
                let possibilities: Vec<String> =
                    matches.iter().map(|o| format!("'--{}'", o.name)).collect();
                eprintln!(
                    "{}: option '{}' is ambiguous; possibilities: {}",
                    go.progname(),
                    arg.to_string_lossy(),
                    possibilities.join(" ")
                );
                go.optind += 1;
                go.nextchar = None;
                return b'?' as i32;
            }
            let m = matches[0];
            // C: *longind = option_index;
            go.option_index = longopts
                .iter()
                .position(|o| o.name == m.name && o.has_arg == m.has_arg)
                .unwrap_or(0);
            go.long_name = m.name.to_string();

            if m.has_arg == required_argument {
                if let Some(v) = attached {
                    go.optarg = Some(OsString::from_vec(v));
                    go.optind += 1;
                } else if go.optind + 1 < go.args.len() {
                    go.optarg = Some(go.args[go.optind + 1].clone());
                    go.optind += 2;
                } else {
                    eprintln!("{}: option '--{}' requires an argument", go.progname(), m.name);
                    go.optind += 1;
                    go.nextchar = None;
                    return b'?' as i32;
                }
            } else {
                if attached.is_some() {
                    eprintln!(
                        "{}: option '--{}' doesn't allow an argument",
                        go.progname(),
                        m.name
                    );
                    go.optind += 1;
                    go.nextchar = None;
                    return b'?' as i32;
                }
                go.optarg = None;
                go.optind += 1;
            }
            go.nextchar = None;
            return m.val as i32;
        }

        // C: short option cluster ("-abc"), resuming mid-cluster like
        // glibc keeps nextchar across getopt_long() calls.
        let cluster = &bytes[1..];
        let pos = go.nextchar.take().unwrap_or(0);
        if pos >= cluster.len() {
            // Cluster exhausted: advance past it and look at the next arg.
            go.optind += 1;
            continue;
        }
        let c = cluster[pos];

        let idx = optstring.bytes().position(|b| b == c);
        let idx = match idx {
            Some(i) => i,
            None => {
                eprintln!("{}: invalid option -- '{}'", go.progname(), lossy(&[c]));
                go.optopt = c;
                go.optind += 1;
                go.nextchar = None;
                return b'?' as i32;
            }
        };
        let takes_arg = optstring.as_bytes().get(idx + 1) == Some(&b':');
        if takes_arg {
            if pos + 1 < cluster.len() {
                // C: optarg = rest of the cluster ("-sfoo").
                go.optarg = Some(OsString::from_vec(cluster[pos + 1..].to_vec()));
                go.optind += 1;
                go.nextchar = None;
                return c as i32;
            } else if go.optind + 1 < go.args.len() {
                // C: optarg = next argv element ("-s foo").
                go.optarg = Some(go.args[go.optind + 1].clone());
                go.optind += 2;
                go.nextchar = None;
                return c as i32;
            } else {
                eprintln!(
                    "{}: option requires an argument -- '{}'",
                    go.progname(),
                    lossy(&[c])
                );
                go.optopt = c;
                go.optind += 1;
                go.nextchar = None;
                return b'?' as i32;
            }
        }
        // No argument: continue with the next char of the cluster.
        go.nextchar = Some(pos + 1);
        return c as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_cluster_and_permutation() {
        let mut go = Getopt::new(vec![
            OsString::from("i3-msg"),
            OsString::from("-q"),
            OsString::from("nop"),
            OsString::from("-m"),
        ]);
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), b'q' as i32);
        assert_eq!(go.optarg, None);
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), b'm' as i32);
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), -1);
        // GNU permutation: the non-option is returned as remaining arg.
        assert_eq!(go.remaining(), vec![OsString::from("nop")]);
    }

    #[test]
    fn short_option_with_attached_and_next_arg() {
        let mut go = Getopt::new(vec![
            OsString::from("i3-msg"),
            OsString::from("-s/tmp/x"),
            OsString::from("-t"),
            OsString::from("get_tree"),
        ]);
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), b's' as i32);
        assert_eq!(go.optarg, Some(OsString::from("/tmp/x")));
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), b't' as i32);
        assert_eq!(go.optarg, Some(OsString::from("get_tree")));
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), -1);
    }

    #[test]
    fn long_option_abbreviation_and_eq_value() {
        let longopts = vec![option { name: "socket", has_arg: required_argument, val: b's' }];
        let mut go = Getopt::new(vec![OsString::from("i3-msg"), OsString::from("--sock=/x")]);
        assert_eq!(getopt_long(&mut go, "", &longopts), b's' as i32);
        assert_eq!(go.optarg, Some(OsString::from("/x")));
    }

    #[test]
    fn unknown_short_option_returns_question_mark() {
        let mut go = Getopt::new(vec![OsString::from("i3-msg"), OsString::from("-z")]);
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), b'?' as i32);
        assert_eq!(go.optopt, b'z');
    }

    #[test]
    fn double_dash_stops_parsing() {
        let mut go = Getopt::new(vec![
            OsString::from("i3-msg"),
            OsString::from("--"),
            OsString::from("-q"),
        ]);
        assert_eq!(getopt_long(&mut go, "s:t:vhqmr", &[]), -1);
        assert_eq!(go.remaining(), vec![OsString::from("-q")]);
    }

    #[test]
    fn missing_required_arg() {
        let longopts = vec![option { name: "socket", has_arg: required_argument, val: b's' }];
        let mut go = Getopt::new(vec![OsString::from("i3-msg"), OsString::from("--socket")]);
        assert_eq!(getopt_long(&mut go, "", &longopts), b'?' as i32);
    }
}