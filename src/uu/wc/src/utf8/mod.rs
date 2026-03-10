// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeResult {
    Char(char, usize),
    Invalid(usize),
    Incomplete,
}

#[inline]
pub fn expected_utf8_len(first: u8) -> Option<usize> {
    match first {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

#[inline]
pub fn decode_char(input: &[u8]) -> DecodeResult {
    let Some((&first, rest)) = input.split_first() else {
        return DecodeResult::Incomplete;
    };

    match first {
        0x00..=0x7F => DecodeResult::Char(first.into(), 1),
        0xC2..=0xDF => {
            if rest.is_empty() {
                return DecodeResult::Incomplete;
            }
            let b1 = rest[0];
            if b1 & 0xC0 != 0x80 {
                return DecodeResult::Invalid(1);
            }
            let scalar = ((u32::from(first & 0x1F)) << 6) | u32::from(b1 & 0x3F);
            DecodeResult::Char(char::from_u32(scalar).expect("valid 2-byte UTF-8"), 2)
        }
        0xE0..=0xEF => {
            if rest.len() < 2 {
                return DecodeResult::Incomplete;
            }
            let b1 = rest[0];
            let b2 = rest[1];
            let valid_b1 = match first {
                0xE0 => (0xA0..=0xBF).contains(&b1),
                0xED => (0x80..=0x9F).contains(&b1),
                _ => (0x80..=0xBF).contains(&b1),
            };
            if !valid_b1 || b2 & 0xC0 != 0x80 {
                return DecodeResult::Invalid(1);
            }
            let scalar = ((u32::from(first & 0x0F)) << 12)
                | ((u32::from(b1 & 0x3F)) << 6)
                | u32::from(b2 & 0x3F);
            DecodeResult::Char(char::from_u32(scalar).expect("valid 3-byte UTF-8"), 3)
        }
        0xF0..=0xF4 => {
            if rest.len() < 3 {
                return DecodeResult::Incomplete;
            }
            let b1 = rest[0];
            let b2 = rest[1];
            let b3 = rest[2];
            let valid_b1 = match first {
                0xF0 => (0x90..=0xBF).contains(&b1),
                0xF4 => (0x80..=0x8F).contains(&b1),
                _ => (0x80..=0xBF).contains(&b1),
            };
            if !valid_b1 || b2 & 0xC0 != 0x80 || b3 & 0xC0 != 0x80 {
                return DecodeResult::Invalid(1);
            }
            let scalar = ((u32::from(first & 0x07)) << 18)
                | ((u32::from(b1 & 0x3F)) << 12)
                | ((u32::from(b2 & 0x3F)) << 6)
                | u32::from(b3 & 0x3F);
            DecodeResult::Char(char::from_u32(scalar).expect("valid 4-byte UTF-8"), 4)
        }
        _ => DecodeResult::Invalid(1),
    }
}
