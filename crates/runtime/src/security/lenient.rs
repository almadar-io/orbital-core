//! `Buffer.from(text, 'base64')`: the lenient base64 decoder the JS-twin providers share.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

/// Lenient about padding and whitespace; skips characters outside the standard alphabet.
pub fn base64_decode_lenient(text: &str) -> Vec<u8> {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/'))
        .collect();
    let trimmed = cleaned.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in trimmed.chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            _ => 63,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_and_whitespace_are_optional() {
        assert_eq!(base64_decode_lenient("TWFu"), b"Man");
        assert_eq!(base64_decode_lenient("TWE="), b"Ma");
        assert_eq!(base64_decode_lenient("TW E"), b"Ma");
        assert_eq!(base64_decode_lenient("TWE"), b"Ma");
    }

    #[test]
    fn foreign_characters_are_skipped_and_empty_is_empty() {
        assert_eq!(base64_decode_lenient("T*W-Fu"), b"Man");
        assert!(base64_decode_lenient("").is_empty());
    }
}
