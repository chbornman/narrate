//! Identity primitives: `ContentHash` (BLAKE3-256), `EventId`/`SessionId`
//! (ULID newtypes), `UtcMillis` timestamps, and the monotonic `Minter`.
//!
//! Contract: spec/EVENTS.md §1.

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use ulid::Ulid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdError {
    #[error("content hash must be 64 lowercase hex characters")]
    InvalidContentHash,
    #[error("id must be a 26-character uppercase Crockford-base32 ULID")]
    InvalidUlid,
    #[error("timestamp must be RFC 3339 UTC with exactly millisecond precision and Z")]
    InvalidTimestamp,
    #[error("device id must be 32 lowercase hex characters")]
    InvalidDeviceId,
}

/// BLAKE3-256 of a file's bytes, stored as 64 lowercase hex characters.
///
/// Uppercase or mixed-case input is rejected, never silently normalized
/// (spec/EVENTS.md §1.1: a silent normalizer hides corrupted producers).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn from_hex(s: &str) -> Result<Self, IdError> {
        if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(s.to_owned()))
        } else {
            Err(IdError::InvalidContentHash)
        }
    }

    pub fn from_bytes_of(content: &[u8]) -> Self {
        Self(blake3::hash(content).to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 26 characters, Crockford base32, uppercase (spec/EVENTS.md §1.2).
/// The Crockford alphabet excludes I, L, O, U; the first character is
/// `0..=7` so the value fits 128 bits.
// pub(crate): the collections store (crate::collections) validates the
// same ULID surface for collection/note ids without minting a newtype.
pub(crate) fn valid_ulid_str(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 26
        && (b'0'..=b'7').contains(&b[0])
        && b.iter().all(|c| {
            matches!(c,
                b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')
        })
}

macro_rules! ulid_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn from_str_strict(s: &str) -> Result<Self, IdError> {
                if valid_ulid_str(s) {
                    Ok(Self(s.to_owned()))
                } else {
                    Err(IdError::InvalidUlid)
                }
            }

            pub fn from_ulid(u: Ulid) -> Self {
                Self(u.to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

ulid_newtype! {
    /// Event identity (spec/EVENTS.md §1.2). Lexicographic order over the
    /// canonical 26-char form equals numeric ULID order; `id` is log order.
    EventId
}

ulid_newtype! {
    /// Session identity (spec/EVENTS.md §9): a ULID minted at session start.
    SessionId
}

/// UTC wall-clock timestamp with millisecond precision.
///
/// Wire form is RFC 3339 with exactly millisecond precision and `Z`
/// (spec/EVENTS.md §1.3): `2026-06-09T14:23:05.123Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UtcMillis(i64);

impl UtcMillis {
    pub fn from_epoch_ms(ms: i64) -> Self {
        Self(ms)
    }

    pub fn epoch_ms(self) -> i64 {
        self.0
    }

    pub fn now() -> Self {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before 1970")
            .as_millis();
        Self(ms as i64)
    }

    /// RFC 3339 UTC with exactly millisecond precision and `Z`.
    pub fn to_rfc3339(self) -> String {
        let days = self.0.div_euclid(86_400_000);
        let rem = self.0.rem_euclid(86_400_000);
        let (y, m, d) = civil_from_days(days);
        let h = rem / 3_600_000;
        let min = (rem / 60_000) % 60;
        let s = (rem / 1_000) % 60;
        let ms = rem % 1_000;
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.{ms:03}Z")
    }

    /// Strict parse: exactly the canonical 24-byte form. Field ranges are
    /// validated, so the representation is unique and round-trips byte-exact.
    pub fn parse(s: &str) -> Result<Self, IdError> {
        let b = s.as_bytes();
        if b.len() != 24
            || b[4] != b'-'
            || b[7] != b'-'
            || b[10] != b'T'
            || b[13] != b':'
            || b[16] != b':'
            || b[19] != b'.'
            || b[23] != b'Z'
        {
            return Err(IdError::InvalidTimestamp);
        }
        let num = |range: std::ops::Range<usize>| -> Result<i64, IdError> {
            let mut v: i64 = 0;
            for &c in &b[range] {
                if !c.is_ascii_digit() {
                    return Err(IdError::InvalidTimestamp);
                }
                v = v * 10 + i64::from(c - b'0');
            }
            Ok(v)
        };
        let y = num(0..4)?;
        let m = num(5..7)?;
        let d = num(8..10)?;
        let h = num(11..13)?;
        let min = num(14..16)?;
        let sec = num(17..19)?;
        let ms = num(20..23)?;
        if !(1..=12).contains(&m)
            || d < 1
            || d > days_in_month(y, m as u32)
            || h > 23
            || min > 59
            || sec > 59
        {
            return Err(IdError::InvalidTimestamp);
        }
        let days = days_from_civil(y, m as u32, d);
        Ok(Self(
            days * 86_400_000 + h * 3_600_000 + min * 60_000 + sec * 1_000 + ms,
        ))
    }
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date from days since 1970-01-01 (inverse of `days_from_civil`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl fmt::Display for UtcMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

/// Id + timestamp fixed at capture onset; insertion may happen later
/// (spec/EVENTS.md §1.2, §10).
#[derive(Debug, Clone)]
pub struct Minted {
    pub id: EventId,
    pub ts: UtcMillis,
}

const RAND80_MASK: u128 = (1u128 << 80) - 1;

/// Monotonic ULID minter (spec/EVENTS.md §1.2): each issued id is strictly
/// greater than the last, by incrementing the random component within a
/// millisecond and clamping the timestamp component to
/// `max(last_issued_ms, now_ms)` if the wall clock regresses.
#[derive(Debug, Default)]
pub struct Minter {
    last: Mutex<Option<u128>>,
    warned_regression: AtomicBool,
}

impl Minter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint from the system clock. `ts` is testimony (actual wall clock);
    /// `id` is order (clamped, strictly increasing).
    pub fn mint(&self) -> Minted {
        self.mint_at(UtcMillis::now())
    }

    /// Mint with an explicit wall-clock reading (testability; the clamp and
    /// regression rules apply exactly as for `mint`).
    pub fn mint_at(&self, now: UtcMillis) -> Minted {
        let now_ms = now.epoch_ms().max(0) as u64;
        let mut last = self.last.lock().expect("minter mutex poisoned");
        let value = match *last {
            None => fresh_ulid_value(now_ms),
            Some(prev) => {
                let prev_ms = (prev >> 80) as u64;
                if now_ms > prev_ms {
                    fresh_ulid_value(now_ms)
                } else {
                    // Same millisecond or clock regression: clamp the
                    // timestamp component and increment (§1.3: if the wall
                    // clock regresses > 60 s, log a warning; do nothing else).
                    if prev_ms.saturating_sub(now_ms) > 60_000
                        && !self.warned_regression.swap(true, Ordering::Relaxed)
                    {
                        eprintln!(
                            "photoproof-core: wall clock regressed {} ms; \
                             event ids remain monotonic (spec/EVENTS.md §1.3)",
                            prev_ms - now_ms
                        );
                    }
                    prev + 1
                }
            }
        };
        *last = Some(value);
        Minted {
            id: EventId::from_ulid(Ulid(value)),
            ts: now,
        }
    }
}

fn fresh_ulid_value(ms: u64) -> u128 {
    ((ms as u128) << 80) | (Ulid::new().0 & RAND80_MASK)
}

/// Device id length per spec/EVENTS.md §9: 32 lowercase hex characters.
/// Published so the shell that MINTS ids (settings.rs truncates a blake3
/// hex to this length) and this validator provably agree — a mismatch
/// would silently re-mint a fresh id on every launch.
pub const DEVICE_ID_LEN: usize = 32;

/// `device_id` per spec/EVENTS.md §9: 32 lowercase hex, random per install.
pub fn validate_device_id(s: &str) -> Result<(), IdError> {
    if s.len() == DEVICE_ID_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(IdError::InvalidDeviceId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_rejects_uppercase() {
        let upper = "B3A91C0D5E7F20146AA8C3D9E1F5B2640C7D8E9F1A2B3C4D5E6F708192A3B4C5";
        assert_eq!(
            ContentHash::from_hex(upper),
            Err(IdError::InvalidContentHash)
        );
        assert!(ContentHash::from_hex(&upper.to_lowercase()).is_ok());
    }

    #[test]
    fn content_hash_round_trips_bytes() {
        let h = ContentHash::from_bytes_of(b"photoproof");
        assert_eq!(h.as_str().len(), 64);
        assert!(ContentHash::from_hex(h.as_str()).is_ok());
    }

    #[test]
    fn event_id_rejects_lowercase_and_bad_chars() {
        assert!(EventId::from_str_strict("01JZ5C4R2GW8Q1T9M3N7P5XKDA").is_ok());
        assert!(EventId::from_str_strict("01jz5c4r2gw8q1t9m3n7p5xkda").is_err());
        assert!(EventId::from_str_strict("01JZ5C4R2GW8Q1T9M3N7P5XKDI").is_err()); // I excluded
        assert!(EventId::from_str_strict("81JZ5C4R2GW8Q1T9M3N7P5XKDA").is_err()); // > 128 bits
        assert!(EventId::from_str_strict("01JZ5C4R2GW8Q1T9M3N7P5XKD").is_err()); // 25 chars
    }

    #[test]
    fn utc_millis_round_trip_and_strictness() {
        let t = UtcMillis::parse("2026-06-09T14:23:05.123Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-06-09T14:23:05.123Z");
        assert!(UtcMillis::parse("2026-06-09T14:23:05Z").is_err());
        assert!(UtcMillis::parse("2026-06-09T14:23:05.1234Z").is_err());
        assert!(UtcMillis::parse("2026-06-09T14:23:05.123+00:00").is_err());
    }
}
