//! Dependency-free helpers for the `prop_*` property tests.
//!
//! A tiny deterministic PRNG (SplitMix64) drives the random inputs, so every
//! failure is reproducible from the printed seed and case number:
//!
//! ```sh
//! PROP_SEED=0x1234 PROP_CASES=50000 cargo test --locked --test prop_config
//! ```
#![allow(dead_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};

const DEFAULT_SEED: u64 = 0x5745_4952_4547_5541; // fixed so CI is reproducible
const DEFAULT_CASES: u64 = 2_000;

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish value in `0..n` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn chance(&mut self, numerator: usize, denominator: usize) -> bool {
        self.below(denominator) < numerator
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64() as u8).collect()
    }

    /// Arbitrary text biased toward characters that matter to the parsers.
    pub fn messy_text(&mut self, max_len: usize) -> String {
        const INTERESTING: &[char] = &[
            '[', ']', '=', '#', ',', ':', '/', '%', '.', '-', '_', '+', ' ', '\t', '\n', '\r',
            '\0', '\u{1}', '\u{7f}', '\u{a0}', '\u{2003}', '\u{feff}', 'é', 'İ', '0', '9', 'a',
            'Z',
        ];
        let len = self.below(max_len + 1);
        (0..len)
            .map(|_| match self.below(4) {
                0 => *self.pick(INTERESTING),
                1 => char::from_u32(self.below(0x11_0000) as u32).unwrap_or('?'),
                _ => (b' ' + self.below(95) as u8) as char,
            })
            .collect()
    }

    /// Random ASCII casing of `word` ("PrivateKey" -> "pRIVATEkEY").
    pub fn recase(&mut self, word: &str) -> String {
        word.chars()
            .map(|c| {
                if self.chance(1, 2) {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    }

    pub fn spaces(&mut self) -> &'static str {
        self.pick::<&str>(&["", " ", "  ", "\t", " \t "])
    }
}

/// A canonical WireGuard key: base64 of 32 random bytes (44 chars, one `=`).
pub fn wg_key(rng: &mut Rng) -> String {
    base64(&rng.bytes(32))
}

pub fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn env_u64(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    let raw = raw.trim();
    match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => raw.parse().ok(),
    }
}

/// Run `property` for `PROP_CASES` random cases. Each case gets its own
/// PRNG derived from `PROP_SEED`, the property name and the case index, and a
/// failing case reports exactly how to replay it.
pub fn check(name: &str, property: impl Fn(&mut Rng)) {
    let seed = env_u64("PROP_SEED").unwrap_or(DEFAULT_SEED);
    let cases = env_u64("PROP_CASES").unwrap_or(DEFAULT_CASES);
    let name_hash = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    for case in 0..cases {
        let mut rng = Rng::new(seed ^ name_hash ^ case.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| property(&mut rng))) {
            let msg = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            panic!(
                "property `{name}` failed at case {case} (replay: PROP_SEED={seed:#x}, \
                 case {case} of PROP_CASES>={}): {msg}",
                case + 1
            );
        }
    }
}
