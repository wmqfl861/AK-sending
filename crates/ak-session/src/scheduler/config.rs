//! Scheduler policy: two-dimensional concurrency modes plus resource caps.
//!
//! Frozen semantics (contract §2/§3):
//!
//! | `same_code_mode` | `cross_code_mode` | behavior |
//! |---|---|---|
//! | `serial` (default) | `parallel` (default) | at most one active task per code, codes run in parallel |
//! | `parallel` | `parallel` | everything parallel, bounded by caps |
//! | `serial` | `serial` | a single active code group with a single task |
//! | `parallel` | `serial` | a single active code group with parallel tasks inside |
//!
//! Serial mode overrides the matching parallel cap (a serial dimension
//! behaves as a cap of 1). Both ends intersect their policies: if either
//! side requires serial, the effective mode is serial.
//!
//! The JSON form parses the frozen field names of contract §3. Wave 19
//! models the three knobs the scheduler needs (`max_active_tasks_per_code`,
//! `max_active_code_groups` — the number of active code connections — and
//! `max_queued_tasks`); the remaining frozen fields
//! (`max_active_tasks`, `max_queued_tasks_per_code`,
//! `max_pending_admissions`) are validated for range and accepted for
//! forward compatibility but not stored, because they govern runtime-side
//! bounds (pending-admission handshakes, per-code queue admission) that
//! the B6 runtime layers on top of this model.
use std::fmt;

/// Default per-code active-task cap (contract §3 sample).
pub const DEFAULT_MAX_ACTIVE_TASKS_PER_CODE: u32 = 4;
/// Default active code-connection cap (contract §3 sample).
pub const DEFAULT_MAX_ACTIVE_CODE_CONNECTIONS: u32 = 4;
/// Default local queue bound (contract §3 sample).
pub const DEFAULT_MAX_QUEUED_TASKS: u32 = 256;
/// Upper bound for `max_active_tasks_per_code` (contract §3).
pub const MAX_ACTIVE_TASKS_PER_CODE: u32 = 16;
/// Upper bound for `max_active_code_connections` / `max_active_code_groups`
/// (contract §3).
pub const MAX_ACTIVE_CODE_CONNECTIONS: u32 = 16;
/// Upper bound for `max_queued_tasks` (contract §3).
pub const MAX_QUEUED_TASKS: u32 = 4096;

/// Concurrency of tasks sharing one [`CodeGroupKey`](crate::scheduler::key::CodeGroupKey).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SameCodeMode {
    /// At most one active task per code at a time.
    Serial,
    /// Up to `max_active_tasks_per_code` active tasks per code.
    Parallel,
}

impl SameCodeMode {
    /// Parse the frozen JSON word (`"serial"` / `"parallel"`).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "serial" => Some(SameCodeMode::Serial),
            "parallel" => Some(SameCodeMode::Parallel),
            _ => None,
        }
    }

    /// The frozen JSON word.
    pub fn as_str(self) -> &'static str {
        match self {
            SameCodeMode::Serial => "serial",
            SameCodeMode::Parallel => "parallel",
        }
    }
}

impl fmt::Display for SameCodeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Concurrency of tasks across different code groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossCodeMode {
    /// Only one active code group at a time (the group may hold several
    /// tasks under `same_code_mode = parallel`).
    Serial,
    /// Up to `max_active_code_connections` active code groups.
    Parallel,
}

impl CrossCodeMode {
    /// Parse the frozen JSON word (`"serial"` / `"parallel"`).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "serial" => Some(CrossCodeMode::Serial),
            "parallel" => Some(CrossCodeMode::Parallel),
            _ => None,
        }
    }

    /// The frozen JSON word.
    pub fn as_str(self) -> &'static str {
        match self {
            CrossCodeMode::Serial => "serial",
            CrossCodeMode::Parallel => "parallel",
        }
    }
}

impl fmt::Display for CrossCodeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a policy is rejected (parsing or range validation).
///
/// Dynamic strings taken from input are truncated and stripped of control
/// characters before being embedded, so hostile JSON cannot smuggle escape
/// sequences into log or UI output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// The document is not a strict JSON object of the expected shape.
    #[error("malformed policy JSON: {detail}")]
    MalformedJson {
        /// What was wrong, in bounded sanitized text.
        detail: String,
    },
    /// A field name outside the frozen set.
    #[error("unknown policy field: {field}")]
    UnknownField {
        /// Sanitized field name.
        field: String,
    },
    /// The same field appears twice.
    #[error("duplicate policy field: {field}")]
    DuplicateField {
        /// Sanitized field name.
        field: String,
    },
    /// A mode field holds a value other than `"serial"`/`"parallel"`.
    #[error("invalid mode for {field}: {value}")]
    InvalidMode {
        /// Field name.
        field: &'static str,
        /// Sanitized value.
        value: String,
    },
    /// A limit field holds a non-natural number.
    #[error("invalid limit for {field}: {detail}")]
    InvalidLimit {
        /// Field name.
        field: &'static str,
        /// What was wrong, in bounded sanitized text.
        detail: String,
    },
    /// A limit is outside its frozen range.
    #[error("policy limit out of range: {field}={value}")]
    OutOfRange {
        /// Field name.
        field: &'static str,
        /// The offending value.
        value: u32,
    },
}

/// Bound and sanitize an input-derived string so it is safe for display.
fn sanitize_token(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(40)
        .map(|c| match c as u32 {
            0x00..=0x1F | 0x7F..=0x9F => '?',
            _ => c,
        })
        .collect();
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// The scheduling policy for one session runtime.
///
/// Constructing via [`SchedulerPolicy::new`] or [`SchedulerPolicy::from_json`]
/// guarantees the frozen ranges; `Default` follows the contract defaults
/// (`serial`/`parallel`, 4/4/256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchedulerPolicy {
    /// Concurrency for tasks sharing one code group.
    pub same_code_mode: SameCodeMode,
    /// Concurrency across code groups.
    pub cross_code_mode: CrossCodeMode,
    /// Active tasks allowed per code group (1..=16; 1 in effect under
    /// same-code serial).
    pub max_active_tasks_per_code: u32,
    /// Active code groups (code connections) allowed (1..=16; 1 in effect
    /// under cross-code serial). JSON name: `max_active_code_groups`.
    pub max_active_code_connections: u32,
    /// Local queue bound (1..=4096).
    pub max_queued_tasks: u32,
}

impl Default for SchedulerPolicy {
    fn default() -> Self {
        SchedulerPolicy {
            same_code_mode: SameCodeMode::Serial,
            cross_code_mode: CrossCodeMode::Parallel,
            max_active_tasks_per_code: DEFAULT_MAX_ACTIVE_TASKS_PER_CODE,
            max_active_code_connections: DEFAULT_MAX_ACTIVE_CODE_CONNECTIONS,
            max_queued_tasks: DEFAULT_MAX_QUEUED_TASKS,
        }
    }
}

/// Effective numeric limits after serial-mode overrides (contract §2:
/// serial overrides the matching parallel cap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveLimits {
    /// Active tasks per group, 1 under same-code serial.
    pub max_tasks_per_group: u32,
    /// Active code groups, 1 under cross-code serial.
    pub max_active_groups: u32,
}

impl SchedulerPolicy {
    /// Build a policy, validating the frozen ranges.
    pub fn new(
        same_code_mode: SameCodeMode,
        cross_code_mode: CrossCodeMode,
        max_active_tasks_per_code: u32,
        max_active_code_connections: u32,
        max_queued_tasks: u32,
    ) -> Result<Self, PolicyError> {
        let policy = SchedulerPolicy {
            same_code_mode,
            cross_code_mode,
            max_active_tasks_per_code,
            max_active_code_connections,
            max_queued_tasks,
        };
        policy.validate()?;
        Ok(policy)
    }

    /// Check every field against the frozen ranges.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if !(1..=MAX_ACTIVE_TASKS_PER_CODE).contains(&self.max_active_tasks_per_code) {
            return Err(PolicyError::OutOfRange {
                field: "max_active_tasks_per_code",
                value: self.max_active_tasks_per_code,
            });
        }
        if !(1..=MAX_ACTIVE_CODE_CONNECTIONS).contains(&self.max_active_code_connections) {
            return Err(PolicyError::OutOfRange {
                field: "max_active_code_connections",
                value: self.max_active_code_connections,
            });
        }
        if !(1..=MAX_QUEUED_TASKS).contains(&self.max_queued_tasks) {
            return Err(PolicyError::OutOfRange {
                field: "max_queued_tasks",
                value: self.max_queued_tasks,
            });
        }
        Ok(())
    }

    /// Numeric limits in effect: a serial dimension behaves as a cap of 1.
    pub fn effective_limits(&self) -> EffectiveLimits {
        EffectiveLimits {
            max_tasks_per_group: match self.same_code_mode {
                SameCodeMode::Serial => 1,
                SameCodeMode::Parallel => self.max_active_tasks_per_code,
            },
            max_active_groups: match self.cross_code_mode {
                CrossCodeMode::Serial => 1,
                CrossCodeMode::Parallel => self.max_active_code_connections,
            },
        }
    }

    /// Intersect two ends' policies (contract §2: if either side requires
    /// serial, the effective mode is serial; limits take the minimum).
    pub fn intersect(&self, other: &SchedulerPolicy) -> SchedulerPolicy {
        let merged = SchedulerPolicy {
            same_code_mode: match (self.same_code_mode, other.same_code_mode) {
                (SameCodeMode::Serial, _) | (_, SameCodeMode::Serial) => SameCodeMode::Serial,
                (SameCodeMode::Parallel, SameCodeMode::Parallel) => SameCodeMode::Parallel,
            },
            cross_code_mode: match (self.cross_code_mode, other.cross_code_mode) {
                (CrossCodeMode::Serial, _) | (_, CrossCodeMode::Serial) => CrossCodeMode::Serial,
                (CrossCodeMode::Parallel, CrossCodeMode::Parallel) => CrossCodeMode::Parallel,
            },
            max_active_tasks_per_code: self
                .max_active_tasks_per_code
                .min(other.max_active_tasks_per_code),
            max_active_code_connections: self
                .max_active_code_connections
                .min(other.max_active_code_connections),
            max_queued_tasks: self.max_queued_tasks.min(other.max_queued_tasks),
        };
        // Both inputs are in-range, so the minimums are too; validate anyway
        // so the invariant "every constructed policy is valid" is checked.
        merged
            .validate()
            .expect("intersection of valid policies stays valid");
        merged
    }

    /// Serialize to the frozen JSON field names, compact, fixed field order.
    ///
    /// Only the five modeled fields are written; see the module docs for the
    /// forward-compatibility fields [`SchedulerPolicy::from_json`] accepts.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"same_code_mode\":\"{}\",\"cross_code_mode\":\"{}\",\
             \"max_active_tasks_per_code\":{},\"max_active_code_groups\":{},\
             \"max_queued_tasks\":{}}}",
            self.same_code_mode.as_str(),
            self.cross_code_mode.as_str(),
            self.max_active_tasks_per_code,
            self.max_active_code_connections,
            self.max_queued_tasks,
        )
    }

    /// Parse a strict JSON object with the frozen field names.
    ///
    /// Missing fields fall back to the contract defaults; unknown fields,
    /// duplicate keys, escapes, floats, negative numbers, non-object
    /// documents and trailing garbage are rejected. The extra frozen fields
    /// (`max_active_tasks` 1..=64, `max_queued_tasks_per_code` 1..=512 and
    /// not above `max_queued_tasks`, `max_pending_admissions` 1..=64) are
    /// range-checked and accepted but not stored.
    pub fn from_json(text: &str) -> Result<Self, PolicyError> {
        let mut parser = Parser::new(text.as_bytes());
        parser.skip_ws();
        parser.expect_byte(b'{')?;

        let mut policy = SchedulerPolicy::default();
        let mut seen_same = false;
        let mut seen_cross = false;
        let mut seen_per_code = false;
        let mut seen_connections = false;
        let mut seen_queued = false;

        parser.skip_ws();
        if parser.peek() == Some(b'}') {
            parser.bump();
        } else {
            loop {
                parser.skip_ws();
                let key = parser.parse_string()?;
                parser.skip_ws();
                parser.expect_byte(b':')?;
                parser.skip_ws();
                match key.as_str() {
                    "same_code_mode" => {
                        reject_duplicate(seen_same, "same_code_mode")?;
                        seen_same = true;
                        policy.same_code_mode =
                            parse_mode_field("same_code_mode", &mut parser, SameCodeMode::parse)?;
                    }
                    "cross_code_mode" => {
                        reject_duplicate(seen_cross, "cross_code_mode")?;
                        seen_cross = true;
                        policy.cross_code_mode =
                            parse_mode_field("cross_code_mode", &mut parser, CrossCodeMode::parse)?;
                    }
                    "max_active_tasks_per_code" => {
                        reject_duplicate(seen_per_code, "max_active_tasks_per_code")?;
                        seen_per_code = true;
                        policy.max_active_tasks_per_code = parse_limit_field(
                            "max_active_tasks_per_code",
                            &mut parser,
                            1,
                            u64::from(MAX_ACTIVE_TASKS_PER_CODE),
                        )?;
                    }
                    "max_active_code_groups" => {
                        reject_duplicate(seen_connections, "max_active_code_groups")?;
                        seen_connections = true;
                        policy.max_active_code_connections = parse_limit_field(
                            "max_active_code_connections",
                            &mut parser,
                            1,
                            u64::from(MAX_ACTIVE_CODE_CONNECTIONS),
                        )?;
                    }
                    "max_queued_tasks" => {
                        reject_duplicate(seen_queued, "max_queued_tasks")?;
                        seen_queued = true;
                        policy.max_queued_tasks = parse_limit_field(
                            "max_queued_tasks",
                            &mut parser,
                            1,
                            u64::from(MAX_QUEUED_TASKS),
                        )?;
                    }
                    // Forward-compatible frozen fields: validated, not stored.
                    "max_active_tasks" => {
                        parse_limit_field("max_active_tasks", &mut parser, 1, 64)?;
                    }
                    "max_queued_tasks_per_code" => {
                        let queued_cap = if seen_queued {
                            policy.max_queued_tasks
                        } else {
                            DEFAULT_MAX_QUEUED_TASKS
                        };
                        parse_limit_field(
                            "max_queued_tasks_per_code",
                            &mut parser,
                            1,
                            u64::from(queued_cap.min(512)),
                        )?;
                    }
                    "max_pending_admissions" => {
                        parse_limit_field("max_pending_admissions", &mut parser, 1, 64)?;
                    }
                    other => {
                        return Err(PolicyError::UnknownField {
                            field: sanitize_token(other),
                        })
                    }
                }
                parser.skip_ws();
                match parser.peek() {
                    Some(b',') => {
                        parser.bump();
                    }
                    Some(b'}') => {
                        parser.bump();
                        break;
                    }
                    _ => {
                        return Err(PolicyError::MalformedJson {
                            detail: "expected ',' or '}' after field value".to_owned(),
                        })
                    }
                }
            }
        }

        parser.skip_ws();
        if parser.remaining() {
            return Err(PolicyError::MalformedJson {
                detail: "trailing bytes after policy object".to_owned(),
            });
        }
        policy.validate()?;
        Ok(policy)
    }
}

fn reject_duplicate(seen: bool, field: &'static str) -> Result<(), PolicyError> {
    if seen {
        return Err(PolicyError::DuplicateField {
            field: field.to_owned(),
        });
    }
    Ok(())
}

/// Strict cursor over the policy JSON subset (object, ASCII strings without
/// escapes, natural numbers).
struct Parser<'a> {
    text: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a [u8]) -> Self {
        Parser { text, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.text.get(self.pos).copied()
    }

    fn bump(&mut self) {
        self.pos += 1;
    }

    fn remaining(&self) -> bool {
        self.pos < self.text.len()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.bump();
        }
    }

    fn expect_byte(&mut self, byte: u8) -> Result<(), PolicyError> {
        if self.peek() == Some(byte) {
            self.bump();
            Ok(())
        } else {
            Err(PolicyError::MalformedJson {
                detail: format!("expected '{}'", byte as char),
            })
        }
    }

    /// One JSON string: no escapes (the frozen schema needs none), no raw
    /// control characters.
    fn parse_string(&mut self) -> Result<String, PolicyError> {
        self.expect_byte(b'"')?;
        let start = self.pos;
        loop {
            match self.peek() {
                Some(b'"') => break,
                Some(b'\\') => {
                    return Err(PolicyError::MalformedJson {
                        detail: "string escapes are not accepted".to_owned(),
                    })
                }
                Some(byte) if byte >= 0x20 => self.bump(),
                _ => {
                    return Err(PolicyError::MalformedJson {
                        detail: "unterminated or control character in string".to_owned(),
                    })
                }
            }
        }
        let raw = &self.text[start..self.pos];
        self.bump(); // closing quote
        std::str::from_utf8(raw)
            .map(|s| s.to_owned())
            .map_err(|_| PolicyError::MalformedJson {
                detail: "field name is not valid UTF-8".to_owned(),
            })
    }

    /// One natural number (no sign, no exponent, no fraction, JSON
    /// canonical form without leading zeros).
    fn parse_number(&mut self) -> Result<u64, PolicyError> {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.bump();
        }
        if start == self.pos {
            return Err(PolicyError::MalformedJson {
                detail: "expected a digit".to_owned(),
            });
        }
        let digits = &self.text[start..self.pos];
        if digits.len() > 1 && digits[0] == b'0' {
            return Err(PolicyError::MalformedJson {
                detail: "leading zeros are not accepted".to_owned(),
            });
        }
        let mut value: u64 = 0;
        for &digit in digits {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(digit - b'0')))
                .ok_or_else(|| PolicyError::MalformedJson {
                    detail: "number out of range".to_owned(),
                })?;
        }
        Ok(value)
    }
}

/// Parse a mode value; `field` names the key for error reporting.
fn parse_mode_field<T>(
    field: &'static str,
    parser: &mut Parser<'_>,
    parse_word: fn(&str) -> Option<T>,
) -> Result<T, PolicyError> {
    let value = parser.parse_string()?;
    parse_word(&value).ok_or_else(|| PolicyError::InvalidMode {
        field,
        value: sanitize_token(&value),
    })
}

/// Parse a limit value with its frozen range.
///
/// Values that do not fit in `u32` are [`PolicyError::InvalidLimit`];
/// values that fit but fall outside the frozen `[min, max]` range are
/// [`PolicyError::OutOfRange`].
fn parse_limit_field(
    field: &'static str,
    parser: &mut Parser<'_>,
    min: u64,
    max: u64,
) -> Result<u32, PolicyError> {
    if !matches!(parser.peek(), Some(b'0'..=b'9')) {
        return Err(PolicyError::MalformedJson {
            detail: "limit must be a natural number".to_owned(),
        });
    }
    let value = parser.parse_number()?;
    if value > u64::from(u32::MAX) {
        return Err(PolicyError::InvalidLimit {
            field,
            detail: format!("value {value} does not fit in u32"),
        });
    }
    let value = value as u32;
    if u64::from(value) < min || u64::from(value) > max {
        return Err(PolicyError::OutOfRange { field, value });
    }
    Ok(value)
}
