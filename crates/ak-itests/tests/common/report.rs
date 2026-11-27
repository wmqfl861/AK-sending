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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

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
