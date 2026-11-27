//! What a failure report may show of text captured from a child process
//! (Q-N3 recurrence, 2026-11-27; reviews 19 to 21).
//!
//! [`redact`] is for what a failure prints. The callers that parse a line
//! (the code, the bound port, the candidates) keep the raw text. Hidden:
//! the task code in every form the parser takes; hex runs of 32 or more
//! (a registry fingerprint, a pairing id); paths and file names, quoted or
//! not, spaces included; the source root name the receiver prints;
//! addresses that are not loopback (their ports stay). The rules are about
//! shapes and a few introducing words, so they cannot see a file name that
//! sits in an ordinary sentence, and only ports are printed, so two
//! loopback targets with the same port are told apart nowhere here.
//!
//! A path never runs on over the words that follow it into the next line.
//! After an unquoted path, a word keeps belonging to it until an explicit
//! delimiter (a clause closer, an `=`, a flag or an opening bracket, a
//! trailing colon) — or, when there is no punctuation at all, until a known
//! system error phrase that reaches the end of the line, so that the cause
//! after such a path is kept.

use std::io::{BufRead, BufReader, Read, Write as IoWrite};
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// The reportable form of captured text, line by line.
pub fn redact(text: &str) -> String {
    text.split('\n')
        .map(redact_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `c` may sit in front of a secret inside one word. A quote is
/// one only when it is closed later in the line (`'` also ends names and
/// English words): `later` is the rest of the line after it.
fn is_opener(c: char, later: &str) -> bool {
    match c {
        '"' | '“' => true,
        '\'' => later.contains('\''),
        '(' | '[' | '{' | '<' | '（' => true,
        _ => false,
    }
}

/// What closes an opening quote.
fn closing_quote(open: char) -> Option<char> {
    match open {
        '"' => Some('"'),
        '\'' => Some('\''),
        '“' => Some('”'),
        _ => None,
    }
}

/// What ends a path or a name inside one word: what follows it there is
/// other text.
fn is_clause_closer(c: char) -> bool {
    matches!(c, ',' | ';' | ')' | ']' | '}' | '=' | '）' | '＝')
}

/// Punctuation after a secret, kept so the line still reads.
fn is_trailing(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | ',' | ';' | ')' | ']' | '}' | ':' | '.' | '!' | '?' | '”' | '）'
    )
}

/// The words after which a bare file name (`in device.key`) or a quoted
/// name (`under "photos"`) is a name on disk, not a word of the message.
const INTRODUCES_A_NAME: [&str; 15] = [
    "in", "at", "into", "under", "dir", "path", "file", "db", "key", "open", "read", "write",
    "save", "load", "create",
];

/// The system error phrases that end an unquoted path when no punctuation
/// does. ASCII, any case; a trailing period and ` (os error N)` may follow.
const SYSTEM_ERRORS: [&str; 5] = [
    "permission denied",
    "access is denied",
    "no such file or directory",
    "the system cannot find the file specified",
    "the system cannot find the path specified",
];

/// Whether `text`, from a word boundary to the end of the line, is one of
/// [`SYSTEM_ERRORS`], with what may follow it.
fn is_system_error_to_end(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let Some(mut rest) = SYSTEM_ERRORS
        .iter()
        .find_map(|phrase| lower.strip_prefix(phrase))
    else {
        return false;
    };
    rest = rest.strip_prefix('.').unwrap_or(rest);
    match rest.strip_prefix(" (os error ") {
        Some(digits) => digits
            .strip_suffix(')')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())),
        None => rest.is_empty(),
    }
}

/// `true` for text that is a filesystem path: a drive root, a UNC root,
/// any `/` root (`/private`, `/var/cache/x`), a path from here (`..\x`,
/// `./x`), anything with a backslash, a URL, or with a `/` and a file name
/// as its last part (`src/main.rs`; not prose such as `Y/n`).
fn looks_like_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive_root = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    let from_here = ["./", "../", ".\\", "..\\"]
        .iter()
        .any(|start| text.starts_with(start));
    let last = text.rsplit(['\\', '/']).next().unwrap_or(text);
    drive_root
        || text.starts_with('/')
        || text.starts_with(r"\\")
        || from_here
        || text.contains('\\')
        || text.contains("://")
        || (text.contains('/') && is_bare_file_name(last))
}

/// A file name with no separator in it (`device.key`): a dot and a short
/// extension that starts with a letter (`256.00` and `1.5s` are numbers).
fn is_bare_file_name(text: &str) -> bool {
    let Some((base, extension)) = text.rsplit_once('.') else {
        return false;
    };
    let short = !extension.is_empty()
        && extension.len() <= 4
        && extension.bytes().all(|b| b.is_ascii_alphanumeric());
    !base.is_empty() && short && extension.as_bytes()[0].is_ascii_alphabetic()
}

/// An address as text; a zone id (`fe80::1%12`) is left out of the parse.
fn parse_host(text: &str) -> Option<IpAddr> {
    let bare = text.split('%').next()?;
    if let Ok(v6) = bare.parse::<Ipv6Addr>() {
        return Some(IpAddr::V6(v6));
    }
    bare.parse::<Ipv4Addr>().ok().map(IpAddr::V4)
}

/// Loopback and unspecified addresses, and loopback and unspecified in
/// their v4-mapped form, stay in a report.
fn address_stays(address: IpAddr) -> bool {
    let local = |v4: Ipv4Addr| v4.is_loopback() || v4.is_unspecified();
    match address {
        IpAddr::V4(v4) => local(v4),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || v6.to_ipv4_mapped().is_some_and(local)
        }
    }
}

/// An IP address at the start of some text.
struct Address {
    /// How many bytes it takes there, its port included.
    len: usize,
    /// Whether it is replaced (`false` for loopback and unspecified).
    hidden: bool,
    /// The port's digits when it has one.
    port: Option<String>,
}

/// The length of an address whose port digits start at `start` in its text.
fn port_then(start: usize, digits: &str) -> (usize, Option<String>) {
    let len = digits.bytes().take_while(|b| b.is_ascii_digit()).count();
    (start + len, (len > 0).then(|| digits[..len].to_string()))
}

/// An IP address at the start of `text`. Forms: `[v6]:port` (the bracket
/// belongs to the address), a bare v6, `v4`, `v4:port`. What follows the
/// port is syntax again, so `ip:port: reason` keeps its second colon (the
/// CLI prints `{addr}: {reason}`).
fn address_prefix(text: &str) -> Option<Address> {
    if let Some(after) = text.strip_prefix('[') {
        let end = after.find(']')?;
        let address = parse_host(&after[..end])?;
        if !address.is_ipv6() {
            return None;
        }
        // In `text` the port's digits start after `[`, the address, `]` and `:`.
        let (digits_start, digits) = match after[end + 1..].strip_prefix(':') {
            Some(digits) => (end + 3, digits),
            None => (end + 2, ""),
        };
        let (len, port) = port_then(digits_start, digits);
        return Some(Address {
            len,
            hidden: !address_stays(address),
            port,
        });
    }
    let run = text
        .bytes()
        .take_while(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.' | b'%'))
        .count();
    let candidate = text.get(..run)?;
    if let Some(address) = parse_host(candidate) {
        return Some(Address {
            len: run,
            hidden: !address_stays(address),
            port: None,
        });
    }
    let (host, digits) = candidate.split_once(':')?;
    let address = IpAddr::V4(Ipv4Addr::from_str(host).ok()?);
    let (len, port) = port_then(host.len() + 1, digits);
    Some(Address {
        len,
        hidden: !address_stays(address),
        port,
    })
}

/// A task code in every form the parser takes, from a case-insensitive
/// `AKI1` on: how many bytes it takes. Only the canonical length is read,
/// and the parser's own rules decide, so whatever follows the code — a
/// letter, `-suffix` — is scanned on its own.
fn task_code_len(text: &str) -> Option<usize> {
    if !text.get(..4)?.eq_ignore_ascii_case("AKI1") {
        return None;
    }
    let grouped = text.get(..44).filter(|code| code.as_bytes()[4] == b'-');
    [grouped, text.get(..36)]
        .into_iter()
        .flatten()
        .find(|code| ak_identity::invite::InviteCode::parse(code).is_ok())
        .map(str::len)
}

/// `text` with the task codes and the hex runs of 32 or more hidden,
/// wherever they start. A hex run stops where a code starts, so a
/// fingerprint written right before one does not swallow the code's first
/// letter.
fn redact_plain(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let code = rest
            .char_indices()
            .filter(|(_, c)| c.eq_ignore_ascii_case(&'A'))
            .find_map(|(at, _)| task_code_len(&rest[at..]).map(|len| (at, len)));
        let until = code.map_or(rest.len(), |(at, _)| at);
        hide_hex_runs(&rest[..until], &mut shown);
        let Some((at, len)) = code else {
            return shown;
        };
        shown.push_str("AKI1-<code>");
        rest = &rest[at + len..];
    }
}

fn hide_hex_runs(text: &str, shown: &mut String) {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let run = bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .count();
        if run >= 32 {
            shown.push_str("<hex>");
        } else {
            shown.push_str(&text[at..at + run]);
        }
        at += run;
        let other = bytes[at..]
            .iter()
            .take_while(|b| !b.is_ascii_hexdigit())
            .count();
        shown.push_str(&text[at..at + other]);
        at += other;
    }
}

/// A whole text that a report hides.
#[derive(Clone, Copy)]
enum Whole {
    /// An address: what replaces it.
    Address,
    Path,
    Name,
}

/// Whether `text` (no surrounding punctuation) is one whole secret.
fn whole_secret(text: &str, quoted: bool, previous: &str) -> Option<Whole> {
    if let Some(address) = address_prefix(text) {
        if address.len == text.len() {
            return Some(Whole::Address);
        }
    }
    if looks_like_path(text) {
        return Some(Whole::Path);
    }
    let introduced = INTRODUCES_A_NAME.contains(&previous);
    if introduced && is_bare_file_name(text) {
        return Some(Whole::Path);
    }
    (introduced && quoted).then_some(Whole::Name)
}

/// What replaces [`whole_secret`]'s `text`.
fn show_whole(whole: Whole, text: &str) -> String {
    match whole {
        Whole::Address => {
            let address = address_prefix(text).expect("an address was found here");
            if address.hidden {
                format!(
                    "<addr>{}",
                    address
                        .port
                        .map(|port| format!(":{port}"))
                        .unwrap_or_default()
                )
            } else {
                text[..address.len].to_string()
            }
        }
        Whole::Path => "<path>".to_string(),
        Whole::Name => "<name>".to_string(),
    }
}

/// Whether an unquoted path `stem` (shown with `after` and `tail` behind
/// it) may have more words after it: one whose last part is not a file
/// name may (`C:\Users\John Doe`); one that ends in a file name, or is
/// followed by punctuation, does not.
fn path_runs_on(whole: Whole, stem: &str, after: &str, tail: &str) -> bool {
    let last = stem.rsplit(['\\', '/']).next().unwrap_or(stem);
    matches!(whole, Whole::Path) && after.is_empty() && tail.is_empty() && !is_bare_file_name(last)
}

/// `(stem, tail)`: the text without the punctuation that trails it.
fn strip_trailing(text: &str) -> (&str, &str) {
    let stem_len = text.trim_end_matches(is_trailing).len();
    text.split_at(stem_len)
}

/// Where the first clause closer is in `text`.
fn clause_end(text: &str) -> Option<usize> {
    text.char_indices()
        .find(|(_, c)| is_clause_closer(*c))
        .map(|(at, _)| at)
}

/// What becomes of a word that an unquoted path ran on to.
enum RunEnd {
    /// Still the path: absorbed.
    Path,
    /// The path up to this byte of the word; from there the word is other
    /// text.
    Until(usize),
    /// Not the path: the word is handled on its own.
    Other,
}

/// An unquoted path runs on to the word at `word_at` in `line` until a
/// word that starts a flag or an opening bracket, holds a clause closer,
/// ends with a colon, or starts a known system error phrase that reaches
/// the end of the line. A bracket or closer inside a word that still
/// carries a path separator (`(x86)\Common`) does not end it.
fn run_on_end(word: &str, line: &str, word_at: usize) -> RunEnd {
    if is_system_error_to_end(&line[word_at..]) {
        return RunEnd::Other;
    }
    if let Some('-' | '(' | '[' | '{' | '<' | '"' | '“' | '（') = word.chars().next() {
        return if word.contains(['\\', '/']) {
            RunEnd::Path
        } else {
            RunEnd::Other
        };
    }
    match clause_end(word) {
        Some(at) => {
            let after = word[at..]
                .chars()
                .next()
                .map_or(0, |closer| closer.len_utf8());
            if word[at + after..].contains(['\\', '/']) {
                RunEnd::Path
            } else {
                RunEnd::Until(at)
            }
        }
        None if word.ends_with(':') => RunEnd::Until(word.len() - 1),
        None => RunEnd::Path,
    }
}

/// What one word of a line becomes.
struct Shown {
    text: String,
    /// The word took the line up to here (a quoted name that spans words):
    /// the next word starts there at the earliest.
    until: Option<usize>,
    /// An unquoted path without an end yet: the words that follow belong
    /// to it.
    runs_on: bool,
}

impl Shown {
    fn text(text: String) -> Shown {
        Shown {
            text,
            until: None,
            runs_on: false,
        }
    }
}

/// What the word at `word_at..word_end` of `line` becomes; `previous` is
/// the word shown before it.
fn show_word(word: &str, line: &str, word_at: usize, word_end: usize, previous: &str) -> Shown {
    let lead_end = word
        .char_indices()
        .find(|(at, c)| !is_opener(*c, &line[word_at + at + c.len_utf8()..]))
        .map_or(word.len(), |(at, _)| at);
    let (lead, body) = word.split_at(lead_end);
    // An address, first with its own bracket.
    if let Some(address) = address_prefix(word) {
        return Shown::text(format!(
            "{}{}",
            show_whole(Whole::Address, word),
            redact_plain(&word[address.len..])
        ));
    }
    if !lead.is_empty() {
        if let Some(address) = address_prefix(body) {
            return Shown::text(format!(
                "{lead}{}{}",
                show_whole(Whole::Address, body),
                redact_plain(&body[address.len..])
            ));
        }
    }
    // An opening quote: the quoted text is one, up to its closing quote,
    // which may be in a later word. Commas and spaces inside are part of it.
    if let Some(open) = lead
        .chars()
        .next_back()
        .filter(|open| closing_quote(*open).is_some())
    {
        let closer = closing_quote(open).expect("an opening quote has a closing one");
        let after_word = &line[word_end..];
        let quoted = match body.find(closer) {
            Some(at) => Some((&body[..at], &body[at + closer.len_utf8()..], word_end)),
            None => after_word.find(closer).map(|at| {
                let close_at = word_end + at;
                let after = close_at + closer.len_utf8();
                let stop = line[after..]
                    .find(char::is_whitespace)
                    .map_or(line.len(), |offset| after + offset);
                (
                    &line[word_at + lead_end..close_at],
                    &line[after..stop],
                    stop,
                )
            }),
        };
        return match quoted {
            Some((content, after, until)) => {
                let content = content.trim();
                let whole = whole_secret(content, true, previous)
                    .or_else(|| INTRODUCES_A_NAME.contains(&previous).then_some(Whole::Name));
                let middle = match whole {
                    Some(whole) => show_whole(whole, content),
                    None => redact_plain(content),
                };
                Shown {
                    text: format!("{lead}{middle}{closer}{}", redact_plain(after)),
                    until: (until > word_end).then_some(until),
                    runs_on: false,
                }
            }
            // Never closed: hidden to the end of the line, which starts over.
            None => Shown {
                text: format!("{lead}<unterminated>"),
                until: Some(line.len()),
                runs_on: false,
            },
        };
    }
    // `key=value` and `--flag=value`: the value is where a secret can be.
    if let Some((key, value)) = body.split_once('=') {
        let (stem, tail) = strip_trailing(value);
        if let Some(whole) = whole_secret(stem, false, "") {
            return Shown {
                text: format!(
                    "{lead}{key}={}{}",
                    show_whole(whole, stem),
                    redact_plain(tail)
                ),
                until: None,
                runs_on: path_runs_on(whole, stem, "", tail),
            };
        }
    }
    let (stem, tail) = strip_trailing(body);
    let (stem, after) = match clause_end(stem) {
        Some(at) => (&stem[..at], &stem[at..]),
        None => (stem, ""),
    };
    match whole_secret(stem, false, previous) {
        Some(whole) => Shown {
            text: format!("{lead}{}{after}{tail}", show_whole(whole, stem)),
            until: None,
            runs_on: path_runs_on(whole, stem, after, tail),
        },
        None => Shown::text(format!("{lead}{}", redact_plain(body))),
    }
}

fn redact_line(line: &str) -> String {
    let mut shown = String::with_capacity(line.len());
    let mut previous = String::new();
    // Whether an unquoted path is running on over the words.
    let mut runs_on = false;
    let mut at = 0;
    while at < line.len() {
        let word_at = match line[at..].find(|c: char| !c.is_whitespace()) {
            Some(offset) => at + offset,
            None => break,
        };
        let word_end = line[word_at..]
            .find(char::is_whitespace)
            .map_or(line.len(), |offset| word_at + offset);
        at = word_end;
        let word = &line[word_at..word_end];
        let (shown_word, from_the_path) = if runs_on {
            match run_on_end(word, line, word_at) {
                RunEnd::Path => continue,
                RunEnd::Until(end) => (Shown::text(redact_plain(&word[end..])), true),
                RunEnd::Other => (show_word(word, line, word_at, word_end, &previous), false),
            }
        } else {
            (show_word(word, line, word_at, word_end, &previous), false)
        };
        at = shown_word.until.unwrap_or(word_end);
        runs_on = shown_word.runs_on;
        // A space before the word, unless it is what a path's last word
        // left behind, or it follows the last one without one (the
        // punctuation after a closing quote).
        let attached = from_the_path || !line[..word_at].ends_with(char::is_whitespace);
        if !shown.is_empty() && !attached {
            shown.push(' ');
        }
        shown.push_str(&shown_word.text);
        previous = if shown_word.text.contains('<') {
            String::new()
        } else {
            shown_word
                .text
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_lowercase()
        };
    }
    shown
}

/// The subcommand words a spawn failure may name.
const SUBCOMMANDS: [&str; 6] = [
    "task",
    "code-recv",
    "code-send",
    "join-send",
    "join-recv",
    "registry-server",
];

/// What a spawn failure may say about a child's arguments: long flags by
/// name (`--key=value` as `--key=<value>`) and the subcommand words as
/// they are; every other value hidden.
pub fn argv_summary(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            let name = match arg.split_once('=') {
                Some((flag, _)) => flag,
                None => arg.as_str(),
            };
            if name.starts_with("--") || SUBCOMMANDS.contains(&name) {
                if name.len() == arg.len() {
                    arg.clone()
                } else {
                    format!("{name}=<value>")
                }
            } else {
                "<value>".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- the chain: processes, logs and one frozen report --------------------

pub fn workspace_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/ak-itests has a workspace root two levels up")
        .to_path_buf()
}

pub fn cargo_bin() -> PathBuf {
    if let Some(cargo) = std::env::var_os("CARGO") {
        return PathBuf::from(cargo);
    }
    std::env::var_os("USERPROFILE")
        .map(|home| {
            PathBuf::from(home).join(".cargo").join(if cfg!(windows) {
                "bin\\cargo.exe"
            } else {
                "bin/cargo"
            })
        })
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

/// Path of the `ak-cli` executable (which also provides the
/// `registry-server` subcommand), building it once if needed.
pub fn cli_exe() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let root = workspace_root();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        let exe = target_dir.join("debug").join(if cfg!(windows) {
            "ak-cli.exe"
        } else {
            "ak-cli"
        });
        let output = Command::new(cargo_bin())
            .args(["build", "-p", "ak-cli"])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|err| panic!("cannot run cargo build -p ak-cli: {err}"));
        assert!(
            output.status.success(),
            "cargo build -p ak-cli failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(exe.exists(), "cargo build produced no {}", exe.display());
        exe
    })
    .clone()
}

/// The port a `registry-server --port 0` bound, from its `v2 pairing
/// registry on tls <addr> (db …)` line. The server prints that line only
/// once it listens (2026-11-19 review), so no port is picked here and
/// bound later by another process, and no client starts too early.
pub fn announced_registry_port(line: &str) -> u16 {
    line.split("v2 pairing registry on tls ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .filter(|port| *port != 0)
        .unwrap_or_else(|| panic!("bound address in the registry line: {}", redact(line)))
}

/// Lines a piped stream produced so far, each stamped with the moment the
/// reader thread read it, fed by a reader thread so a full pipe can never
/// block the child. A stdout log also holds its process's stderr and its
/// exit status (from a watcher thread), so a timed-out wait reports all
/// three (2026-11-19 review: a CLI that fails before its first stdout line
/// says why only on stderr).
///
/// Q-N3 recurrence (2026-11-27): a report shows every line of every process
/// of the chain with milliseconds since the first of those lines, how each
/// pipe ended (closed by the writer, or a read error) and each process's
/// exit status, and never prints a task code, a registry fingerprint, a
/// pairing id, a filesystem path or an address that is not loopback.
#[derive(Clone, Default)]
pub struct LineLog {
    pub state: Arc<Mutex<LogState>>,
    pub stderr: Option<Arc<Mutex<LogState>>>,
    /// The process tag, for a report's heading.
    pub name: Option<String>,
    /// What the watcher thread concluded about the process's end.
    pub exit: Option<Arc<Mutex<Watched>>>,
}

/// What the watcher of a child process knows about how it ended.
#[derive(Clone, Default)]
pub enum Watched {
    /// Still running (or not watched at all).
    #[default]
    Running,
    Exited(std::process::ExitStatus),
    /// The watcher could not keep polling the child: its end is unknown.
    Unwatched(String),
}

/// One of a process's two pipes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One line read off a pipe and the moment it was read.
#[derive(Clone)]
pub struct TimedLine {
    pub at_ms: u128,
    pub text: String,
}

#[derive(Default)]
pub struct LogState {
    pub lines: Vec<TimedLine>,
    /// How the pipe ended and when: `None` for the error means the writer
    /// closed it; `None` for the whole field means it is still open.
    pub end: Option<(u128, Option<String>)>,
}

/// The test binary's monotonic clock: every captured line is stamped with
/// milliseconds since the first process was spawned (tests in one binary
/// share it; each report then counts from its own first line).
pub fn since_origin_ms() -> u128 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis()
}

/// How long a report waits for the last lines of a process that already
/// exited: they can still be in the pipes when its exit is first seen
/// (review 20), and the first error is often the last line.
pub const SETTLE: std::time::Duration = std::time::Duration::from_millis(1000);

/// Wait, at most [`SETTLE`], until every process here that already exited
/// has nothing left coming out of its pipes. A process still running is
/// not waited for.
pub fn settle(logs: &[&LineLog]) {
    let until = Instant::now() + SETTLE;
    while Instant::now() < until {
        let pending = logs
            .iter()
            .any(|log| log.exit().is_some() && !log.pipes_ended());
        if !pending {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `logs` with every process once: a log is left out when one of its
/// pipes is a pipe of a log already kept (the bare stderr log of a process
/// whose stdout log is listed). A report shows each process once, however
/// the wait that produced it was phrased.
pub fn unique_processes<'a>(logs: &[(&'a str, &'a LineLog)]) -> Vec<(&'a str, &'a LineLog)> {
    pub fn pipes(log: &LineLog) -> impl Iterator<Item = &Arc<Mutex<LogState>>> {
        std::iter::once(&log.state).chain(log.stderr.as_ref())
    }
    let mut kept: Vec<(&str, &LineLog)> = Vec::new();
    for &(name, log) in logs {
        let same = |state: &Arc<Mutex<LogState>>| {
            kept.iter()
                .any(|(_, seen)| pipes(seen).any(|pipe| Arc::ptr_eq(pipe, state)))
        };
        if !pipes(log).any(same) {
            kept.push((name, log));
        }
    }
    kept
}

/// One process as a report froze it.
pub struct FrozenProcess {
    pub name: String,
    pub out: Vec<TimedLine>,
    pub err: Vec<TimedLine>,
    /// How each pipe ended and when: the error is `None` when the writer
    /// closed the pipe, the whole field `None` while it is still open.
    pub out_end: Option<(u128, Option<String>)>,
    pub err_end: Option<(u128, Option<String>)>,
    pub exit: Watched,
}

/// Every process of a chain, taken once for one report (plan of 2026-11-28,
/// after review 21): the last lines of processes that already exited are
/// waited for, then every pipe, every exit status and the common origin
/// are frozen, and the registry database is sampled once. A summary, the
/// details and whatever a test decides all read this one copy, so they
/// cannot disagree. Lines that only arrived during the wait explain a
/// failure; they never turn it into a success.
pub struct Frozen {
    pub origin: u128,
    pub processes: Vec<FrozenProcess>,
    /// The registry's stderr since the cursor, redacted.
    pub decisions: Vec<String>,
    /// The snapshot's look at the transactions the decisions name.
    pub transactions: String,
    pub snapshot: String,
    pub snapshot_at_ms: u128,
}

pub fn freeze(logs: &[(&str, &LineLog)]) -> Frozen {
    let processes = unique_processes(logs);
    settle(&processes.iter().map(|(_, log)| *log).collect::<Vec<_>>());
    let frozen: Vec<FrozenProcess> = processes
        .iter()
        .map(|(name, log)| {
            let out = log
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let err = log.stderr.as_ref().map(|state| {
                state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            FrozenProcess {
                name: name.to_string(),
                out: out.lines.clone(),
                err: err
                    .as_ref()
                    .map(|err| err.lines.clone())
                    .unwrap_or_default(),
                out_end: out.end.clone(),
                err_end: err.as_ref().and_then(|err| err.end.clone()),
                exit: log.watched(),
            }
        })
        .collect();
    let origin = frozen
        .iter()
        .flat_map(|process| [&process.out, &process.err])
        .filter_map(|lines| lines.first())
        .map(|line| line.at_ms)
        .min()
        .unwrap_or_else(since_origin_ms);
    let cursor = DECISIONS_FROM.with(|from| from.get());
    let decisions: Vec<String> = registry_err_lines()
        .into_iter()
        .skip(cursor)
        .map(|line| redact(&line.text))
        .collect();
    let prefixes = pairing_prefixes_in(&decisions);
    let aliases: Vec<String> = (1..=prefixes.len())
        .map(|index| format!("pairing-attempt-{index}"))
        .collect();
    let about: Vec<(&str, [u8; 4])> = aliases
        .iter()
        .zip(&prefixes)
        .map(|(alias, prefix)| (alias.as_str(), *prefix))
        .collect();
    let transactions = REGISTRY_DB
        .with(|slot| slot.borrow().clone())
        .filter(|_| !about.is_empty())
        .map(|db| registry_snapshot_about(&db, &about))
        .unwrap_or_default();
    Frozen {
        origin,
        processes: frozen,
        decisions,
        transactions,
        snapshot: registry_snapshot(),
        snapshot_at_ms: since_origin_ms(),
    }
}

impl Frozen {
    pub fn process(&self, name: &str) -> Option<&FrozenProcess> {
        self.processes.iter().find(|process| process.name == name)
    }

    /// The report's text: each process once, as a timeline of both pipes
    /// counted from the first line any of them wrote, then the registry
    /// database with the moment it was sampled.
    pub fn render(&self) -> String {
        let mut report = String::new();
        for process in &self.processes {
            report.push_str(&format!(
                "\n--- {} (milliseconds since the first line of this report):\n",
                process.name
            ));
            report.push_str(&process.timeline(self.origin).join("\n"));
        }
        if !self.decisions.is_empty() {
            report.push_str(&format!(
                "\n--- registry decisions and notices since the cursor ({} line(s)):\n{}",
                self.decisions.len(),
                self.decisions.join("\n")
            ));
        }
        if !self.transactions.is_empty() {
            report.push_str(&format!(
                "\n--- the transactions those decisions name, looked up by their pairing's first four bytes:\n{}",
                self.transactions
            ));
        }
        report.push_str(&format!(
            "\n--- registry database, read-only snapshot at +{} ms:\n{}",
            self.snapshot_at_ms.saturating_sub(self.origin),
            self.snapshot
        ));
        report
    }
}

impl FrozenProcess {
    /// The lines of both pipes, redacted, each with milliseconds since
    /// `origin` and the stream it came from; then how each pipe ended and
    /// the exit status (still-open pipes last).
    pub fn timeline(&self, origin: u128) -> Vec<String> {
        let mut rows: Vec<(u128, String)> = Vec::new();
        for (state, ended, tag) in [
            (&self.out, &self.out_end, "out"),
            (&self.err, &self.err_end, "err"),
        ] {
            for line in state {
                rows.push((
                    line.at_ms,
                    format!(
                        "[+{:>7} ms] {tag}: {}",
                        line.at_ms.saturating_sub(origin),
                        redact(&line.text)
                    ),
                ));
            }
            match ended {
                Some((at, None)) => rows.push((
                    *at,
                    format!(
                        "[+{:>7} ms] {tag}: <pipe closed by the writer>",
                        at.saturating_sub(origin)
                    ),
                )),
                Some((at, Some(error))) => rows.push((
                    *at,
                    format!(
                        "[+{:>7} ms] {tag}: <pipe lost on a read error: {error}>",
                        at.saturating_sub(origin)
                    ),
                )),
                None => rows.push((u128::MAX, format!("[+      ? ms] {tag}: <pipe still open>"))),
            }
        }
        match &self.exit {
            Watched::Exited(status) if status.success() => {
                rows.push((u128::MAX, format!("exit status: {status}")))
            }
            Watched::Exited(status) => rows.push((u128::MAX - 1, format!("exit status: {status}"))),
            Watched::Unwatched(error) => rows.push((
                u128::MAX - 1,
                format!("exit status: unknown; the watcher could not poll the child: {error}"),
            )),
            Watched::Running => rows.push((
                u128::MAX,
                "exit status: <process still running>".to_string(),
            )),
        }
        rows.sort_by_key(|(at, _)| *at);
        rows.into_iter().map(|(_, text)| text).collect()
    }

    pub fn out_text(&self) -> String {
        Self::text_of(&self.out)
    }

    pub fn err_text(&self) -> String {
        Self::text_of(&self.err)
    }

    pub fn text_of(lines: &[TimedLine]) -> String {
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl LineLog {
    pub fn spawn_reader(stream: impl Read + Send + 'static) -> Self {
        let log = LineLog::default();
        let sink = Arc::clone(&log.state);
        std::thread::spawn(move || {
            let mut read_error = None;
            for line in BufReader::new(stream).lines() {
                match line {
                    Ok(text) => {
                        let at_ms = since_origin_ms();
                        let mut state = sink.lock().unwrap_or_else(|p| p.into_inner());
                        state.lines.push(TimedLine { at_ms, text });
                    }
                    Err(error) => {
                        read_error = Some(error.to_string());
                        break;
                    }
                }
            }
            // The pipe ended: closed by the writer, or on a read error.
            let mut state = sink.lock().unwrap_or_else(|p| p.into_inner());
            state.end = Some((since_origin_ms(), read_error));
        });
        log
    }

    /// Report `stderr` (the same process's other stream) on a timeout.
    pub fn with_stderr(mut self, stderr: &LineLog) -> Self {
        self.stderr = Some(Arc::clone(&stderr.state));
        self
    }

    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    pub fn watched_by(mut self, exit: &Arc<Mutex<Watched>>) -> Self {
        self.exit = Some(Arc::clone(exit));
        self
    }

    /// A log that no pipe feeds: the caller states its lines, whether its
    /// pipes ended, and what the watcher concluded — for synthetic reports.
    pub fn synthetic(
        name: &str,
        out: &[String],
        err: &[String],
        ended: bool,
        exit: Watched,
    ) -> LineLog {
        let now = since_origin_ms();
        let state = |lines: &[String]| -> Arc<Mutex<LogState>> {
            Arc::new(Mutex::new(LogState {
                lines: lines
                    .iter()
                    .map(|text| TimedLine {
                        at_ms: now,
                        text: text.clone(),
                    })
                    .collect(),
                end: ended.then_some((now, None)),
            }))
        };
        LineLog {
            state: state(out),
            stderr: Some(state(err)),
            name: Some(name.to_string()),
            exit: Some(Arc::new(Mutex::new(exit))),
        }
    }

    /// What the watcher concluded so far.
    pub fn watched(&self) -> Watched {
        self.exit
            .as_ref()
            .map(|watched| watched.lock().unwrap_or_else(|p| p.into_inner()).clone())
            .unwrap_or(Watched::Running)
    }

    /// The lines of `stream` so far, for the callers that parse them.
    pub fn lines_of(&self, stream: Stream) -> Vec<TimedLine> {
        let state = match stream {
            Stream::Stdout => &self.state,
            Stream::Stderr => self.stderr.as_ref().unwrap_or(&self.state),
        };
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .lines
            .clone()
    }

    /// This stream's raw lines so far.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines_of(Stream::Stdout)
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    /// The process's exit status once the watcher saw it.
    pub fn exit(&self) -> Option<std::process::ExitStatus> {
        match self.watched() {
            Watched::Exited(status) => Some(status),
            _ => None,
        }
    }

    /// When the first line of this log or its stderr was read.
    pub fn earliest_ms(&self) -> Option<u128> {
        [Stream::Stdout, Stream::Stderr]
            .into_iter()
            .filter_map(|stream| self.lines_of(stream).first().map(|line| line.at_ms))
            .min()
    }

    /// Both pipes ended (closed or on a read error).
    pub fn pipes_ended(&self) -> bool {
        let ended = |state: &Arc<Mutex<LogState>>| {
            state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .end
                .is_some()
        };
        let err = self.stderr.as_ref().map(ended).unwrap_or(true);
        ended(&self.state) && err
    }

    /// The process is over and did not end well: it exited unsuccessfully,
    /// or both its pipes ended after an error line — one condition or the
    /// other, whichever the watcher and the reader threads report first.
    /// An exit status of 0 does not settle it by itself: a process can
    /// print its error and exit 0. It cannot produce the line a wait is
    /// looking for, so the wait stops and reports now.
    pub fn ended_with_error(&self) -> bool {
        let exited_badly = matches!(self.watched(), Watched::Exited(status) if !status.success());
        exited_badly || (self.pipes_ended() && self.has_error_line())
    }

    /// `Error:` or a panic somewhere in this log or its stderr.
    pub fn has_error_line(&self) -> bool {
        let has = |state: &Arc<Mutex<LogState>>| {
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .lines
                .iter()
                .any(|line| line.text.contains("Error:") || line.text.contains("panicked at"))
        };
        has(&self.state) || self.stderr.as_ref().is_some_and(has)
    }

    /// This process as one timeline counted from its earliest line. The
    /// last lines of a process that already exited are waited for (see
    /// [`settle`]).
    pub fn report(&self) -> String {
        freeze(&[(self.name.as_deref().unwrap_or("this process"), self)]).render()
    }

    /// First line containing `needle`, polling until it appears.
    pub fn wait_for(&self, needle: &str, timeout: std::time::Duration) -> String {
        self.wait_for_stream(Stream::Stdout, needle, timeout, &[])
    }

    /// [`Self::wait_for`], with `others`' output (stderr included) in the
    /// timeout report too: a wait on one process often runs out because
    /// of another (2026-11-20 review of batch Q: a Windows CI run timed
    /// out waiting for the receiver's offer line, and the report held only
    /// the receiver's side).
    pub fn wait_for_reporting(
        &self,
        needle: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> String {
        self.wait_for_stream(Stream::Stdout, needle, timeout, others)
    }

    /// [`Self::wait_for`] on one stream of this process. The stream only
    /// decides where the line is looked for; the report, when the wait
    /// gives up, always comes from this process — both pipes, one exit
    /// status — and from every other process this test thread still
    /// holds, those named in `others` and those not, each once. The wait
    /// stops as soon as this process or one of `others` is over and did
    /// not end well (Q-N3, 2026-11-27): the registry is still alive then,
    /// so its log and a read-only snapshot of its database go into the
    /// report. A line that was read after the deadline shows in that
    /// report and does not count as the line having come in time.
    pub fn wait_for_stream(
        &self,
        stream: Stream,
        needle: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> String {
        let deadline = Instant::now() + timeout;
        let deadline_ms = since_origin_ms() + timeout.as_millis();
        loop {
            if let Some(line) = self
                .lines_of(stream)
                .into_iter()
                .find(|line| line.at_ms <= deadline_ms && line.text.contains(needle))
            {
                return line.text;
            }
            let now = Instant::now();
            if self.giving_up(deadline, now, others) {
                self.give_up(stream, &format!("containing {needle:?}"), timeout, others);
            }
            std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(50)));
        }
    }

    /// Wait until at least `count` lines of stdout contain `needle`; a wait
    /// that gives up reports as [`Self::wait_for_stream`] does.
    pub fn wait_count(&self, needle: &str, count: usize, timeout: std::time::Duration) {
        let deadline = Instant::now() + timeout;
        let deadline_ms = since_origin_ms() + timeout.as_millis();
        loop {
            let have = self
                .lines_of(Stream::Stdout)
                .into_iter()
                .filter(|line| line.at_ms <= deadline_ms && line.text.contains(needle))
                .count();
            if have >= count {
                return;
            }
            let now = Instant::now();
            if self.giving_up(deadline, now, &[]) {
                self.give_up(
                    Stream::Stdout,
                    &format!("with at least {count} line(s) containing {needle:?}"),
                    timeout,
                    &[],
                );
            }
            std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(50)));
        }
    }

    /// Whether a wait has run out: its deadline, or this process or one of
    /// `others` over and not ended well.
    fn giving_up(&self, deadline: Instant, now: Instant, others: &[(&str, &LineLog)]) -> bool {
        let ended =
            self.ended_with_error() || others.iter().any(|(_, other)| other.ended_with_error());
        now >= deadline || ended
    }

    /// Panic with the report of every process for a wait that ran out.
    fn give_up(
        &self,
        stream: Stream,
        what: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> ! {
        let mut who: Vec<String> = Vec::new();
        if self.ended_with_error() {
            who.push(self.name.as_deref().unwrap_or("this process").to_string());
        }
        who.extend(
            others
                .iter()
                .filter(|(_, other)| other.ended_with_error())
                .map(|(name, _)| name.to_string()),
        );
        let why = if who.is_empty() {
            format!("waited the full {timeout:?}")
        } else {
            format!(
                "stopped early: {} had already ended without success",
                who.join(" and ")
            )
        };
        let stream = match stream {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        };
        let mut logs: Vec<(&str, &LineLog)> =
            vec![(self.name.as_deref().unwrap_or("this process"), self)];
        logs.extend_from_slice(others);
        // Every other process this test thread still holds goes in too: a
        // wait on one process often runs out because of another, whichever
        // was named.
        let live = live_processes();
        logs.extend(
            live.iter()
                .map(|log| (log.name.as_deref().unwrap_or("process"), log)),
        );
        panic!(
            "no line of the {stream} {what} ({why}){}",
            freeze(&logs).render()
        );
    }
}

thread_local! {
    /// The database of the registry this test thread started last, for the
    /// snapshot in a failure report.
    static REGISTRY_DB: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };

    /// Every process this test thread spawned and has not dropped, in spawn
    /// order. A report names them all — also the one a wait gave up on
    /// before its first line (plan of 2026-11-28, section F).
    static LIVE: std::cell::RefCell<Vec<LineLog>> = const { std::cell::RefCell::new(Vec::new()) };

    /// The registry this test thread started, and where its decision lines
    /// are to be read from: a report frozen after [`mark_decision_cursor`]
    /// shows what the registry decided since, and the transactions named
    /// there (plan of 2026-11-28, section F).
    static REGISTRY_LOG: std::cell::RefCell<Option<LineLog>> = const { std::cell::RefCell::new(None) };
    static DECISIONS_FROM: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Every process this test thread spawned and still holds.
fn live_processes() -> Vec<LineLog> {
    LIVE.with(|live| live.borrow().clone())
}

/// The registry's stderr, decision and notice lines among it.
fn registry_err_lines() -> Vec<TimedLine> {
    REGISTRY_LOG.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|log| log.lines_of(Stream::Stderr))
            .unwrap_or_default()
    })
}

/// From here on, every report of this test thread shows what the registry
/// decided after this moment, and the transactions it names by the first
/// four bytes of their pairing id. Taken once a calibration has shown that
/// this registry's lines are being captured, so the natural attempts are
/// read without the calibration's own lines.
pub fn mark_decision_cursor() {
    DECISIONS_FROM.with(|from| from.set(registry_err_lines().len()));
}

/// The report for every process this test thread spawned and still holds,
/// each once, with the registry database of this thread: what a chain test
/// panics with when one of its waits fails.
pub fn live_report() -> String {
    let live = live_processes();
    let logs: Vec<(&str, &LineLog)> = live
        .iter()
        .map(|log| (log.name.as_deref().unwrap_or("process"), log))
        .collect();
    freeze(&logs).render()
}

/// `scripts/t03_pairing_snapshot.py` on the registry database, or the reason
/// it was not taken. The interpreter is `AK_T03_PYTHON` when set; else the
/// usual names are tried from the PATH in turn (`python`, `python3`, `py -3`
/// on Windows; `python3`, `python` elsewhere) and the first that starts is
/// used.
pub fn registry_snapshot() -> String {
    run_snapshot(REGISTRY_DB.with(|slot| slot.borrow().clone()), &[])
}

/// The same snapshot with the transactions whose pairing ids start with
/// given four bytes printed under an alias — one `(alias, prefix)` per
/// pairing a failure is about. A row is printed only when exactly one
/// transaction matches (plan of 2026-11-28, section F); the bytes looked up
/// are never printed.
pub fn registry_snapshot_about(db: &std::path::Path, about: &[(&str, [u8; 4])]) -> String {
    let specs: Vec<String> = about
        .iter()
        .map(|(alias, prefix)| {
            let prefix: String = prefix.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("{alias}={prefix}")
        })
        .collect();
    let mut extra: Vec<&str> = Vec::new();
    for spec in &specs {
        extra.extend(["--about", spec.as_str()]);
    }
    run_snapshot(Some(db.to_path_buf()), &extra)
}

/// The first four bytes of every pairing id that `lines` name as
/// `(pairing XXXXXXXX)`, in order of first appearance. They are what the
/// registry's decision lines print and what a lookup takes.
pub fn pairing_prefixes_in<S: AsRef<str>>(lines: impl IntoIterator<Item = S>) -> Vec<[u8; 4]> {
    let mut prefixes: Vec<[u8; 4]> = Vec::new();
    for line in lines {
        let mut rest = line.as_ref();
        while let Some(at) = rest.find("(pairing ") {
            rest = &rest[at + "(pairing ".len()..];
            let Some(prefix) = rest.get(..8).and_then(|text| {
                let mut bytes = [0u8; 4];
                for (index, slot) in bytes.iter_mut().enumerate() {
                    *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
                }
                Some(bytes)
            }) else {
                continue;
            };
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

fn run_snapshot(db: Option<PathBuf>, extra: &[&str]) -> String {
    let Some(db) = db else {
        return "not taken: no registry database was registered on this thread".into();
    };
    let script = workspace_root()
        .join("scripts")
        .join("t03_pairing_snapshot.py");
    // Without an explicit interpreter the usual names are tried in turn: a
    // machine may have `python` only, `python3` only, or just the Windows
    // `py` launcher. The first one that starts is the one reported, also
    // when the script then fails.
    let candidates: Vec<Vec<std::ffi::OsString>> = match std::env::var_os("AK_T03_PYTHON") {
        Some(python) => vec![vec![python]],
        None if cfg!(windows) => vec![
            vec!["python".into()],
            vec!["python3".into()],
            vec!["py".into(), "-3".into()],
        ],
        None => vec![vec!["python3".into()], vec!["python".into()]],
    };
    let mut outcome = String::from("not taken: no interpreter was tried");
    for candidate in candidates {
        let started = Instant::now();
        let mut command = Command::new(&candidate[0]);
        command
            .args(&candidate[1..])
            .arg("-I")
            .arg(&script)
            .arg(&db)
            .args(extra);
        match command.output() {
            Ok(output) if output.status.success() => {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
            Ok(output) => {
                return format!(
                    "not taken: exit status {} after {:?}; stderr: {}",
                    output.status,
                    started.elapsed(),
                    redact(&String::from_utf8_lossy(&output.stderr))
                );
            }
            Err(error) => outcome = format!("not taken: cannot start the interpreter ({error})"),
        }
    }
    outcome
}

/// What every CLI child of a chain runs with (Q-N3, 2026-11-27): the
/// endpoints' pairing trace on stderr (`AK_PAIR_DIAG` turns it on inside
/// `ak-cli task`; `RUST_LOG` keeps everything else at warnings).
pub const PAIR_DIAG_ENV: &[(&str, &str)] = &[
    ("AK_PAIR_DIAG", "1"),
    ("RUST_LOG", "warn,ak_pair_diag=info"),
];

/// A resident CLI child with captured stdout/stderr and a WRITABLE stdin
/// (the receive consent prompt reads real terminal input). A watcher
/// thread records its exit status, so a wait can tell "ended with status
/// N" from "closed its pipes" (Q-N3).
pub struct Proc {
    /// Behind a lock: the watcher thread polls it, `kill` ends it.
    pub child: Arc<Mutex<Child>>,
    pub stdout: LineLog,
    #[allow(dead_code)]
    pub stderr: LineLog,
    pub stdin: Option<ChildStdin>,
}

impl Proc {
    pub fn spawn(tag: &str, args: &[String]) -> Proc {
        Proc::spawn_with_env(tag, args, PAIR_DIAG_ENV)
    }

    /// [`Self::spawn`] with extra environment variables for the child.
    pub fn spawn_with_env(tag: &str, args: &[String], env: &[(&str, &str)]) -> Proc {
        let cli = cli_exe();
        let mut command = Command::new(&cli);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command
            .spawn()
            .unwrap_or_else(|err| panic!("spawn {tag} ({}): {err}", argv_summary(args)));
        let child = Arc::new(Mutex::new(child));
        let stderr = LineLog::spawn_reader(
            child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .stderr
                .take()
                .expect("stderr piped"),
        )
        .named(&format!("{tag} (stderr)"));
        let exit: Arc<Mutex<Watched>> = Arc::new(Mutex::new(Watched::Running));
        let stdout = LineLog::spawn_reader(
            child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .stdout
                .take()
                .expect("stdout piped"),
        )
        .with_stderr(&stderr)
        .named(tag)
        .watched_by(&exit);
        // Registered from its first moment, before any line of it exists.
        LIVE.with(|live| live.borrow_mut().push(stdout.clone()));
        let stdin = child.lock().unwrap_or_else(|p| p.into_inner()).stdin.take();
        let watched = Arc::clone(&child);
        let tag = tag.to_string();
        std::thread::spawn(move || loop {
            let status = {
                let mut child = watched.lock().unwrap_or_else(|p| p.into_inner());
                child.try_wait()
            };
            match status {
                Ok(Some(status)) => {
                    *exit.lock().unwrap_or_else(|p| p.into_inner()) = Watched::Exited(status);
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                Err(error) => {
                    eprintln!("t03: cannot poll the child {tag}: {error}");
                    // The report says the end is unknown, not "still running".
                    *exit.lock().unwrap_or_else(|p| p.into_inner()) =
                        Watched::Unwatched(error.to_string());
                    break;
                }
            }
        });
        Proc {
            child,
            stdout,
            stderr,
            stdin,
        }
    }

    /// The exit status once the watcher saw it.
    pub fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.stdout.exit()
    }

    /// Wait for a line of one stream of this process; the report, when
    /// the wait gives up, is this process's (both pipes, one exit status)
    /// and `others`'. See [`LineLog::wait_for_stream`].
    pub fn wait_for_line(
        &self,
        stream: Stream,
        needle: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> String {
        self.stdout.wait_for_stream(stream, needle, timeout, others)
    }

    /// This process's stdout and stderr as one timeline.
    pub fn report(&self) -> String {
        let mut text = format!("{}:", self.stdout.name.as_deref().unwrap_or("process"));
        for line in self.stdout.report().split('\n') {
            text.push('\n');
            text.push_str(line);
        }
        text
    }

    /// Answer the consent prompt. The prompt itself is a `print!` without
    /// a newline (so it never forms a readable line) and the CLI DISCARDS
    /// input older than the question — so feed a fresh `y` every 100 ms
    /// from the moment the (newline-terminated) offer line appeared until
    /// the receiver moves on; a `y` that lands after the question started
    /// is genuinely fresh and accepts.
    pub fn consent_yes(&mut self, done_needles: &[&str], budget: std::time::Duration) {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if let Some(stdin) = self.stdin.as_mut() {
                if writeln!(stdin, "y").and_then(|_| stdin.flush()).is_err() {
                    break; // the child is gone
                }
            }
            let log = self.stdout.snapshot().join("\n");
            if done_needles.iter().any(|needle| log.contains(needle)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// Wait until the child exited, and it exited successfully. Its last
    /// lines are read in first: they can still be in the pipes when the
    /// exit is first seen, and the caller reads the log right after this.
    pub fn wait_exit_ok(&mut self, timeout: std::time::Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.exit_status() {
                settle(&[&self.stdout]);
                assert!(
                    status.success(),
                    "cli child exited with {status}; {}",
                    self.report()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "cli child did not exit within {timeout:?}; {}",
                self.report()
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    pub fn kill(&mut self) {
        self.stdin.take();
        let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        LIVE.with(|live| {
            live.borrow_mut()
                .retain(|log| !Arc::ptr_eq(&log.state, &self.stdout.state))
        });
        self.kill();
    }
}

/// The real v2 registry as a subprocess (`ak-cli registry-server`), same
/// shape as the invite-pairing itests.
pub struct RegistryProc {
    /// Owning the process keeps the registry alive (killed on drop).
    #[allow(dead_code)]
    pub proc: Proc,
    pub addr: SocketAddr,
    pub fp_hex: String,
    /// The database this registry writes. A lookup from another thread
    /// takes it explicitly: the thread-local one is its own thread's.
    pub db: PathBuf,
}

impl RegistryProc {
    pub fn spawn(tag: &str, server_dir: &std::path::Path, db: &std::path::Path) -> RegistryProc {
        RegistryProc::spawn_with_log(tag, server_dir, db, "info,ak_signal::server_v2=debug")
    }

    /// [`Self::spawn`] with the registry's `RUST_LOG` given: the tests of
    /// the calibration (plan of 2026-11-28, section F) show that it is red
    /// when the decision lines are not written.
    pub fn spawn_with_log(
        tag: &str,
        server_dir: &std::path::Path,
        db: &std::path::Path,
        rust_log: &str,
    ) -> RegistryProc {
        let args: Vec<String> = [
            "registry-server",
            "--port",
            "0",
            "--db-path",
            &db.to_string_lossy(),
            "--id-dir",
            &server_dir.to_string_lossy(),
        ]
        .iter()
        .map(|text| text.to_string())
        .collect();
        // Q-N3 (2026-11-27): the registry's DEBUG lines say which branch
        // did what (a ghost redeem expiring, a delivery that went
        // nowhere); everything else stays at INFO.
        let proc = Proc::spawn_with_env(tag, &args, &[("RUST_LOG", rust_log)]);
        REGISTRY_DB.with(|slot| *slot.borrow_mut() = Some(db.to_path_buf()));
        REGISTRY_LOG.with(|slot| *slot.borrow_mut() = Some(proc.stdout.clone()));
        let listening = proc.stdout.wait_for(
            "v2 pairing registry on tls ",
            std::time::Duration::from_secs(60),
        );
        let port = announced_registry_port(&listening);
        let line = proc.stdout.wait_for(
            "registry id (pin with --registry-fp): ",
            std::time::Duration::from_secs(60),
        );
        let fp_hex = line
            .rsplit(' ')
            .next()
            .expect("hex id is the last token")
            .to_ascii_lowercase();
        assert_eq!(
            fp_hex.len(),
            64,
            "registry id line: {}",
            redact(line.as_ref())
        );
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        RegistryProc {
            proc,
            addr,
            fp_hex,
            db: db.to_path_buf(),
        }
    }
}
