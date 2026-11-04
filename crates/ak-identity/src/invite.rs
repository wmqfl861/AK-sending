//! Pairing invite codes (`InviteCode`) — the “配对邀请码” of the v2 pairing
//! flow (plan A.2/A.3, `docs/contracts/protocol-v2.md`).
//!
//! An invite code is a **bearer credential that authorizes pairing**. It is
//! a different concept from the legacy six-digit
//! [`crate::pairing_code`] fingerprint check code, which stays unchanged for
//! display-only verification. The new code never enters the old six-digit
//! parsing branch.
//!
//! Frozen format (plan A.2):
//!
//! | item | decision |
//! |---|---|
//! | randomness | 20 bytes from the OS CSPRNG (`OsRng`), 160 bit entropy |
//! | encoding | Crockford Base32, 32 valid symbols |
//! | alphabet | `0123456789ABCDEFGHJKMNPQRSTVWXYZ` |
//! | display | `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` |
//! | normalization | ASCII lowercase folded to uppercase; only dashes at the prescribed group positions, or no grouping at all |
//! | rejection | Unicode homoglyphs, control characters, any whitespace, wrong length, undefined symbols |
//!
//! `AKI1` is a format version and carries no entropy. Domain-separated
//! derivations (plan A.3 §2, labels verbatim):
//!
//! ```text
//! lookup_digest = BLAKE3_DERIVE_KEY("AK invite lookup v1", secret20)
//! auth_key      = BLAKE3_DERIVE_KEY("AK invite authentication v1", secret20)
//! ```
//!
//! The coordination server only ever receives `lookup_digest`; `auth_key`
//! stays on the two endpoints and keys the pairing-transcript MACs
//! ([`crate::pairing_txn`]). Knowing `lookup_digest` does not let an
//! attacker produce those proofs.
//!
//! The code is generated on the client (recommended: client generates,
//! server registers) — never via the clock/counter-derived
//! `ak-signal::util::unique_bytes`. Uniqueness is enforced by the
//! authoritative registry transaction, not by the random draw.
//!
//! [`fmt::Debug`] is hand-written to show only a masked hint, so the full
//! code or raw secret never reaches `{:?}` logs.

use std::fmt;

use rand::{rngs::OsRng, RngCore};

/// Byte length of the random invite-code secret (160 bit entropy).
pub const INVITE_SECRET_LEN: usize = 20;
/// Format-version prefix of every invite code. Not part of the entropy.
pub const INVITE_PREFIX: &str = "AKI1";
/// Number of Crockford Base32 symbols encoding the secret (160 / 5).
pub const INVITE_SYMBOL_COUNT: usize = 32;
/// Crockford Base32 alphabet (I, L, O, U excluded), plan A.2.
const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Domain-separation label for the server-visible lookup digest (plan A.3).
pub const LOOKUP_DIGEST_DOMAIN: &str = "AK invite lookup v1";
/// Domain-separation label for the endpoint-only authentication key
/// (plan A.3).
pub const AUTH_KEY_DOMAIN: &str = "AK invite authentication v1";

/// Why a typed invite code was rejected during normalization/parsing.
///
/// The variants are deliberately distinct so callers can classify entry
/// mistakes (typo, foreign character, wrong copy) without echoing the
/// remaining input back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InviteCodeError {
    #[error("invite code is empty")]
    Empty,
    #[error("invite code must start with the AKI1 version prefix")]
    BadPrefix,
    #[error("invite code has the wrong symbol count")]
    WrongLength,
    #[error("invite code contains a control character (C0/DEL/C1)")]
    ControlCharacter,
    #[error("invite code contains whitespace")]
    Whitespace,
    #[error("invite code contains a non-ASCII character ({0:?}); Unicode homoglyphs are not canonicalized")]
    NonAscii(char),
    #[error("invite code contains an undefined symbol ({0}); the Crockford Base32 alphabet excludes I, L, O and U")]
    UndefinedSymbol(char),
    #[error("invite code dashes are not at the prescribed group positions")]
    BadGrouping,
}

/// A pairing invite code: 20 bytes of OS-CSPRNG entropy plus the frozen
/// `AKI1` Crockford Base32 presentation.
///
/// Holders of the full code can pair with the issuing device; treat stored
/// instances like the device seed (user-private directory, never in logs or
/// exports). Only the derived [`InviteCode::lookup_digest`] is
/// coordination-plane data.
#[derive(Clone)]
pub struct InviteCode {
    secret: [u8; INVITE_SECRET_LEN],
}

impl fmt::Debug for InviteCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: only the masked display hint, never the full code or
        // the raw secret bytes.
        f.debug_struct("InviteCode")
            .field("secret_len", &INVITE_SECRET_LEN)
            .field("hint", &self.code_hint())
            .finish()
    }
}

/// Crockford Base32 value of one (already uppercase ASCII) symbol byte,
/// `None` for anything outside the 32-symbol alphabet.
fn crockford_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'H' => Some(byte - b'A' + 10),
        b'J' | b'K' => Some(byte - b'A' + 9),
        b'M' | b'N' => Some(byte - b'A' + 8),
        b'P'..=b'T' => Some(byte - b'A' + 7),
        b'V'..=b'Z' => Some(byte - b'A' + 6),
        _ => None, // includes the excluded I, L, O, U
    }
}

/// MSB-first Crockford Base32 encoding of the 20 secret bytes. 160 bits
/// divide into exactly 32 symbols; there is no padding or check symbol.
fn crockford_encode(secret: &[u8; INVITE_SECRET_LEN]) -> String {
    let mut out = String::with_capacity(INVITE_SYMBOL_COUNT);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in secret {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((acc >> bits) & 0x1f) as usize;
            out.push(CROCKFORD_ALPHABET[index] as char);
        }
        acc &= (1u32 << bits) - 1;
    }
    out
}

impl InviteCode {
    /// Generate a fresh invite code from the OS CSPRNG (`OsRng`), per plan
    /// A.2. The draw protects against guessing only; global uniqueness is
    /// decided by the authoritative registry `InviteClaim` transaction.
    pub fn generate() -> Self {
        let mut secret = [0u8; INVITE_SECRET_LEN];
        OsRng.fill_bytes(&mut secret);
        InviteCode { secret }
    }

    /// Wrap a known 20-byte secret (e.g. loaded from the local
    /// “pending-registration” secret store).
    pub fn from_secret(secret: [u8; INVITE_SECRET_LEN]) -> Self {
        InviteCode { secret }
    }

    /// The raw secret bytes. Handle like the device seed: user-private
    /// storage only, never logs, exports, or coordination messages.
    pub fn secret(&self) -> &[u8; INVITE_SECRET_LEN] {
        &self.secret
    }

    /// Canonical display form `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX`.
    pub fn to_display(&self) -> String {
        let symbols = crockford_encode(&self.secret);
        let mut out = String::with_capacity(INVITE_PREFIX.len() + 1 + INVITE_SYMBOL_COUNT + 7);
        out.push_str(INVITE_PREFIX);
        for (index, c) in symbols.chars().enumerate() {
            if index % 4 == 0 {
                out.push('-');
            }
            out.push(c);
        }
        out
    }

    /// Normalize and parse a typed or pasted invite code.
    ///
    /// Accepted forms: the canonical grouped layout (dashes only right
    /// after `AKI1` and between the eight four-symbol groups) and the fully
    /// ungrouped `AKI1` + 32 symbols; ASCII lowercase folds to uppercase.
    /// Everything else is rejected with a distinct
    /// [`InviteCodeError`] — in particular Unicode homoglyphs, control
    /// characters, any whitespace, misplaced/partial grouping, wrong length,
    /// and symbols outside the Crockford alphabet (I, L, O, U included).
    /// Crockford's usual I/L/O canonicalization is deliberately *not*
    /// applied: undefined characters are refused, not guessed.
    pub fn parse(text: &str) -> Result<Self, InviteCodeError> {
        if text.is_empty() {
            return Err(InviteCodeError::Empty);
        }
        // Character classes first: only printable ASCII ever reaches the
        // structural checks, so no Unicode homoglyph can masquerade as a
        // valid symbol after case folding. C1 controls (U+0080..=U+009F)
        // are control characters even though they are not ASCII, so that
        // classification runs before the ASCII check.
        let mut upper = String::with_capacity(text.len());
        for c in text.chars() {
            if c.is_ascii_whitespace() {
                return Err(InviteCodeError::Whitespace);
            }
            if crate::is_control_char(c) {
                return Err(InviteCodeError::ControlCharacter);
            }
            if !c.is_ascii() {
                return Err(InviteCodeError::NonAscii(c));
            }
            upper.push(c.to_ascii_uppercase());
        }
        if !upper.starts_with(INVITE_PREFIX) {
            return Err(InviteCodeError::BadPrefix);
        }
        let body = &upper.as_bytes()[INVITE_PREFIX.len()..];

        // Grouping: either no dashes at all, or exactly the canonical
        // layout `-GGGG-GGGG-GGGG-GGGG-GGGG-GGGG-GGGG-GGGG`.
        let symbols: Vec<u8> = if body.contains(&b'-') {
            let rest = match body.split_first() {
                Some((b'-', rest)) => rest,
                _ => return Err(InviteCodeError::BadGrouping),
            };
            let mut collected = Vec::with_capacity(INVITE_SYMBOL_COUNT);
            let mut groups = 0usize;
            for group in rest.split(|&b| b == b'-') {
                if group.len() != 4 {
                    return Err(InviteCodeError::BadGrouping);
                }
                groups += 1;
                collected.extend_from_slice(group);
            }
            if groups != 8 {
                return Err(InviteCodeError::BadGrouping);
            }
            collected
        } else {
            body.to_vec()
        };
        if symbols.len() != INVITE_SYMBOL_COUNT {
            return Err(InviteCodeError::WrongLength);
        }

        // MSB-first 5-bit symbols back into the 20 secret bytes.
        let mut secret = [0u8; INVITE_SECRET_LEN];
        let mut acc: u32 = 0;
        let mut bits: u32 = 0;
        let mut filled = 0usize;
        for &symbol in &symbols {
            let value =
                crockford_value(symbol).ok_or(InviteCodeError::UndefinedSymbol(symbol as char))?;
            acc = (acc << 5) | u32::from(value);
            bits += 5;
            while bits >= 8 {
                bits -= 8;
                secret[filled] = (acc >> bits) as u8;
                filled += 1;
            }
            acc &= (1u32 << bits) - 1;
        }
        if bits != 0 || filled != INVITE_SECRET_LEN {
            return Err(InviteCodeError::WrongLength);
        }
        Ok(InviteCode { secret })
    }

    /// `BLAKE3_DERIVE_KEY("AK invite lookup v1", secret20)` — the only
    /// value the coordination server receives for lookups. It is not a
    /// pairing credential.
    pub fn lookup_digest(&self) -> [u8; 32] {
        blake3::derive_key(LOOKUP_DIGEST_DOMAIN, &self.secret)
    }

    /// `BLAKE3_DERIVE_KEY("AK invite authentication v1", secret20)` — the
    /// endpoint-only MAC key for pairing-transcript proofs
    /// ([`crate::pairing_txn`]). Never send it to the server.
    pub fn auth_key(&self) -> [u8; 32] {
        blake3::derive_key(AUTH_KEY_DOMAIN, &self.secret)
    }

    /// Masked display keeping only the last four symbols:
    /// `AKI1-****-****-****-****-****-****-****-QZ7X`. Safe for logs and
    /// list UIs where the full bearer code must not appear.
    pub fn code_hint(&self) -> String {
        let symbols: Vec<char> = crockford_encode(&self.secret).chars().collect();
        let group_count = symbols.len() / 4;
        let mut out = String::with_capacity(INVITE_PREFIX.len() + group_count * 5);
        out.push_str(INVITE_PREFIX);
        for (index, chunk) in symbols.chunks(4).enumerate() {
            out.push('-');
            if index + 1 == group_count {
                for c in chunk {
                    out.push(*c);
                }
            } else {
                out.push_str("****");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_display_is_canonical_grouped_form() {
        let code = InviteCode::generate();
        let display = code.to_display();
        let groups: Vec<&str> = display.split('-').collect();

        assert_eq!(groups[0], "AKI1");
        assert_eq!(groups.len(), 9, "prefix + 8 groups: {display}");
        assert!(
            groups[1..].iter().all(|group| group.len() == 4),
            "each group has 4 symbols: {display}"
        );
        assert!(
            groups[1..]
                .iter()
                .all(|group| group.bytes().all(|b| crockford_value(b).is_some())),
            "only Crockford symbols appear: {display}"
        );
        assert_eq!(
            display.replace('-', "").len(),
            INVITE_PREFIX.len() + INVITE_SYMBOL_COUNT
        );
    }

    #[test]
    fn parse_roundtrips_display_ungrouped_and_lowercase() {
        let code = InviteCode::generate();
        let display = code.to_display();

        let parsed = InviteCode::parse(&display).expect("canonical display parses");
        assert_eq!(parsed.secret(), code.secret());

        let ungrouped = format!("AKI1{}", &display.replace('-', "")[INVITE_PREFIX.len()..]);
        assert_eq!(
            InviteCode::parse(&ungrouped).unwrap().secret(),
            code.secret(),
            "ungrouped form: {ungrouped}"
        );

        let lower = display.to_lowercase();
        assert_eq!(
            InviteCode::parse(&lower).unwrap().secret(),
            code.secret(),
            "ASCII lowercase folds to uppercase: {lower}"
        );

        assert_eq!(
            InviteCode::from_secret(*code.secret()).to_display(),
            display
        );
    }

    #[test]
    fn parse_rejects_wrong_length_inputs() {
        let display = InviteCode::generate().to_display();
        let ungrouped = display.replace('-', "");

        assert!(matches!(InviteCode::parse(""), Err(InviteCodeError::Empty)));
        assert!(matches!(
            InviteCode::parse("AKI1"),
            Err(InviteCodeError::WrongLength)
        ));
        // Ungrouped, one symbol short / one extra.
        assert!(matches!(
            InviteCode::parse(&ungrouped[..ungrouped.len() - 1]),
            Err(InviteCodeError::WrongLength)
        ));
        assert!(matches!(
            InviteCode::parse(&format!("{ungrouped}0")),
            Err(InviteCodeError::WrongLength)
        ));
        // Grouped, a five-symbol last group is a grouping defect first.
        assert!(matches!(
            InviteCode::parse(&format!("{display}0")),
            Err(InviteCodeError::BadGrouping)
        ));
    }

    #[test]
    fn parse_rejects_undefined_crockford_symbols() {
        for bad in ['I', 'L', 'O', 'U', '#', '$', '.', '+'] {
            let display = InviteCode::generate().to_display();
            let mut chars: Vec<char> = display.chars().collect();
            // Overwrite the first symbol of the second group.
            chars[INVITE_PREFIX.len() + 1] = bad;
            let input: String = chars.into_iter().collect();
            assert!(
                matches!(
                    InviteCode::parse(&input),
                    Err(InviteCodeError::UndefinedSymbol(actual)) if actual == bad
                ),
                "{bad:?} accepted in {input}"
            );
        }
    }

    #[test]
    fn parse_rejects_non_ascii_homoglyphs() {
        for homoglyph in ['О', 'Α', 'Ａ', 'ᛟ', '０', '‍'] {
            let display = InviteCode::generate().to_display();
            let mut chars: Vec<char> = display.chars().collect();
            chars[INVITE_PREFIX.len() + 1] = homoglyph;
            let input: String = chars.into_iter().collect();
            assert!(
                matches!(
                    InviteCode::parse(&input),
                    Err(InviteCodeError::NonAscii(actual)) if actual == homoglyph
                ),
                "homoglyph {homoglyph:?} accepted in {input}"
            );
        }
    }

    #[test]
    fn parse_rejects_control_characters_and_whitespace() {
        for bad in [' ', '\t', '\n', '\r', '\0', '\u{7f}', '\u{9b}', '\u{85}'] {
            let display = InviteCode::generate().to_display();
            let mut chars: Vec<char> = display.chars().collect();
            chars[INVITE_PREFIX.len() + 2] = bad;
            let input: String = chars.into_iter().collect();
            let outcome = InviteCode::parse(&input);
            assert!(
                matches!(
                    &outcome,
                    Err(InviteCodeError::Whitespace) | Err(InviteCodeError::ControlCharacter)
                ),
                "{bad:?} accepted in {input}: {outcome:?}"
            );
            // Leading/trailing whitespace is refused too.
            assert!(InviteCode::parse(&format!("{bad}{display}")).is_err());
            assert!(InviteCode::parse(&format!("{display}{bad}")).is_err());
        }
    }

    #[test]
    fn parse_rejects_wrong_or_missing_prefix() {
        let display = InviteCode::generate().to_display();
        for bad_prefix in ["AKI2", "BKI1", "AKII", "AK1-", "AKI0"] {
            let input = format!("{bad_prefix}{}", &display[INVITE_PREFIX.len()..]);
            assert!(
                matches!(InviteCode::parse(&input), Err(InviteCodeError::BadPrefix)),
                "prefix {bad_prefix} accepted"
            );
        }
        assert!(matches!(
            InviteCode::parse("AK"),
            Err(InviteCodeError::BadPrefix)
        ));
        // Lowercase prefix folds first, so it is fine.
        assert!(InviteCode::parse(&display.to_lowercase()).is_ok());
    }

    #[test]
    fn parse_rejects_misplaced_or_partial_grouping() {
        let display = InviteCode::generate().to_display();
        let symbols = display.replace('-', "");
        let tail = &symbols[INVITE_PREFIX.len()..];

        // Dashes present but the dash right after AKI1 is missing.
        let no_lead_dash = format!("AKI1{}-{}", &tail[..4], &tail[4..]);
        assert!(matches!(
            InviteCode::parse(&no_lead_dash),
            Err(InviteCodeError::BadGrouping)
        ));

        // A three-symbol group in the middle.
        let mut groups: Vec<String> = display.split('-').map(str::to_owned).collect();
        groups[2] = groups[2][..3].to_owned();
        assert!(matches!(
            InviteCode::parse(&groups.join("-")),
            Err(InviteCodeError::BadGrouping)
        ));

        // Double dash after AKI1.
        let double_dash = format!("AKI1--{}", &tail[4..]);
        assert!(matches!(
            InviteCode::parse(&double_dash),
            Err(InviteCodeError::BadGrouping)
        ));

        // Trailing dash.
        assert!(matches!(
            InviteCode::parse(&format!("{display}-")),
            Err(InviteCodeError::BadGrouping)
        ));

        // Seven groups only.
        let mut groups: Vec<String> = display.split('-').map(str::to_owned).collect();
        groups.truncate(8); // prefix + 7 groups
        assert!(matches!(
            InviteCode::parse(&groups.join("-")),
            Err(InviteCodeError::BadGrouping)
        ));
    }

    #[test]
    fn derivations_are_domain_separated_and_stable() {
        let code = InviteCode::generate();

        // Frozen derivation vectors: exactly the two plan A.3 labels.
        assert_eq!(
            code.lookup_digest(),
            blake3::derive_key("AK invite lookup v1", code.secret())
        );
        assert_eq!(
            code.auth_key(),
            blake3::derive_key("AK invite authentication v1", code.secret())
        );
        // The two domains never agree.
        assert_ne!(code.lookup_digest(), code.auth_key());
        // Stable across calls.
        assert_eq!(code.lookup_digest(), code.lookup_digest());
        assert_eq!(code.auth_key(), code.auth_key());

        // A different code derives different values.
        let other = InviteCode::generate();
        assert_ne!(code.lookup_digest(), other.lookup_digest());
        assert_ne!(code.auth_key(), other.auth_key());
    }

    #[test]
    fn derivations_survive_normalization_roundtrip() {
        let code = InviteCode::generate();
        let parsed = InviteCode::parse(&code.to_display()).unwrap();
        assert_eq!(parsed.lookup_digest(), code.lookup_digest());
        assert_eq!(parsed.auth_key(), code.auth_key());
        // A one-bit change in the secret changes both derivations.
        let mut tampered = *code.secret();
        tampered[0] ^= 0x01;
        let tampered = InviteCode::from_secret(tampered);
        assert_ne!(tampered.lookup_digest(), code.lookup_digest());
        assert_ne!(tampered.auth_key(), code.auth_key());
    }

    #[test]
    fn code_hint_masks_all_but_the_last_group() {
        let code = InviteCode::generate();
        let display = code.to_display();
        let hint = code.code_hint();

        let groups: Vec<&str> = display.split('-').collect();
        let hint_groups: Vec<&str> = hint.split('-').collect();
        assert_eq!(hint_groups.len(), groups.len());
        assert_eq!(hint_groups[0], "AKI1");
        for group in &hint_groups[1..8] {
            assert_eq!(*group, "****");
        }
        assert_eq!(hint_groups[8], groups[8], "last group stays visible");
        // No earlier full group leaks into the hint.
        for group in groups[1..8].iter() {
            assert!(!hint.contains(group), "hint leaked group {group}");
        }
    }

    #[test]
    fn debug_output_never_contains_the_code_or_secret() {
        let code = InviteCode::generate();
        let debug = format!("{code:?}");

        assert!(
            !debug.contains(&code.to_display()),
            "Debug leaked the full code: {debug}"
        );
        assert!(
            !debug.contains(&code.to_display().replace('-', "")),
            "Debug leaked the ungrouped code: {debug}"
        );
        assert!(
            !debug.contains(&crate::hex_lower(code.secret())),
            "Debug leaked the raw secret: {debug}"
        );
        assert!(debug.contains("InviteCode") && debug.contains("hint"));
    }

    #[test]
    fn distinct_codes_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..8 {
            let code = InviteCode::generate();
            assert!(seen.insert(code.to_display()), "duplicate code drawn");
        }
    }

    #[test]
    fn crockford_bit_layout_matches_hand_vectors() {
        // All-zero and all-one secrets map to uniform symbols.
        assert_eq!(crockford_encode(&[0u8; INVITE_SECRET_LEN]), "0".repeat(32));
        assert_eq!(crockford_encode(&[0xFF; INVITE_SECRET_LEN]), "Z".repeat(32));

        // 0x08 = 0b00001000: first 5 bits 00001 -> '1', next 5 bits
        // (000 + 00) -> '0'.
        let mut secret = [0u8; INVITE_SECRET_LEN];
        secret[0] = 0x08;
        let encoded = crockford_encode(&secret);
        assert_eq!(&encoded[..2], "10");

        // Decode inverts encode for arbitrary values.
        for seed in [0u64, 1, 0x9E37_79B9_97F4_A7C1, u64::MAX] {
            let mut secret = [0u8; INVITE_SECRET_LEN];
            for (i, slot) in secret.iter_mut().enumerate() {
                *slot = (seed.rotate_left((i as u32) * 7) & 0xFF) as u8;
            }
            let encoded = crockford_encode(&secret);
            let decoded = InviteCode::parse(&format!("AKI1{encoded}")).unwrap();
            assert_eq!(decoded.secret(), &secret, "roundtrip failed for {seed}");
        }
    }
}
