//! `Buffer.from(text, 'base64')`: the lenient base64 decoder the JS-twin providers share.

pub use orbital_core_base::codec::base64_decode_lenient;

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
