//! Lowercase hex encoding for digests that are persisted.
//!
//! `sha2` 0.11 returns a `hybrid_array::Array`, which -- unlike the
//! `GenericArray` of 0.10 -- does not implement `LowerHex`, so the
//! `format!("{:x}", Sha256::digest(..))` this crate used no longer compiles.
//!
//! Both call sites store their result in the database and look rows up by
//! equality against it, so the encoding has to stay byte-for-byte what `{:x}`
//! produced: lowercase, exactly two characters per byte, no separators, no
//! `0x` prefix. That contract is what the tests below pin.

const LOWER_HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

/// Encodes `bytes` as lowercase hexadecimal, two characters per byte.
pub(crate) fn encode_lower(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(LOWER_HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(LOWER_HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::encode_lower;

    #[test]
    fn each_byte_becomes_its_high_nibble_then_its_low_nibble() {
        // 0xa5 distinguishes the two nibbles from each other and from a
        // byte-reversed or nibble-swapped encoding.
        assert_eq!(encode_lower(&[0xa5]), "a5");
    }

    #[test]
    fn a_byte_below_sixteen_keeps_its_leading_zero() {
        // The failure this guards is a variable-width encoding: dropping the
        // leading zero would make `[0x0f, 0xff]` and `[0xff, 0xf0]` collide.
        assert_eq!(encode_lower(&[0x00, 0x0f]), "000f");
    }

    #[test]
    fn the_alphabet_is_lowercase_and_covers_every_nibble() {
        let every_nibble_pair: Vec<u8> = (0..=u8::MAX).collect();
        let encoded = encode_lower(&every_nibble_pair);

        assert_eq!(encoded.len(), 512, "two characters per byte");
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "only lowercase hex digits: {encoded}"
        );
    }

    #[test]
    fn distinct_inputs_encode_distinctly() {
        assert_ne!(encode_lower(&[0x01, 0x23]), encode_lower(&[0x23, 0x01]));
    }
}
