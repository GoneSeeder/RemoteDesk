//! 10-digit, AnyDesk-style addresses without a server of our own.
//!
//! The short ID is a deliberately slow hash of the machine's real public key. Each machine also
//! runs a tiny "alias" endpoint whose key is derived from the short ID; that alias publishes the
//! real public key in n0's free discovery service. A controller derives the alias key from the
//! digits, looks up the real key and re-hashes it: only a key that hashes to the same digits is
//! accepted, so pointing a short ID at another machine means brute-forcing the slow hash.

use iroh::{EndpointId, SecretKey};
use sha2::{Digest, Sha256};

const STRETCH_ROUNDS: u32 = 1 << 21;

/// Slow on purpose (~0.5 s); computed once per machine and once per lookup.
pub fn short_id_for(id: &EndpointId) -> String {
    let mut h: [u8; 32] = Sha256::digest([b"remotedesk-short-v1:".as_slice(), id.as_bytes()].concat()).into();
    for _ in 0..STRETCH_ROUNDS {
        h = Sha256::digest(h).into();
    }
    let n = u64::from_le_bytes(h[..8].try_into().unwrap());
    (1_000_000_000 + n % 9_000_000_000).to_string()
}

pub fn alias_secret(short_id: &str) -> SecretKey {
    let h: [u8; 32] = Sha256::digest(format!("remotedesk-alias-v1:{short_id}")).into();
    SecretKey::from_bytes(&h)
}

/// Strips spaces/dashes; returns the digits if the input is a short ID.
pub fn parse_short(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    (digits.len() == 10 && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits)
}

/// "1610852472" -> "1 610 852 472"
pub fn format(short_id: &str) -> String {
    let mut out = String::new();
    for (i, c) in short_id.chars().enumerate() {
        if i > 0 && (short_id.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}
