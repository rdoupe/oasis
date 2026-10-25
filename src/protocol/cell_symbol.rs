//! Grapheme storage for frame cells.
//!
//! Nearly every cell holds one short grapheme, and frames are rebuilt many
//! times a second, so a heap `String` per cell spent most of a redraw in the
//! allocator. `CellSymbol` keeps up to 24 bytes inline and encodes exactly like
//! `String` (serde and native bincode), so the wire format is unchanged.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

use bincode::de::read::Reader;
use bincode::de::Decoder;
use bincode::error::DecodeError;
use compact_str::CompactString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Longest symbol decoded without a heap allocation.
const INLINE_CAPACITY: usize = std::mem::size_of::<String>();

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellSymbol(CompactString);

impl CellSymbol {
    pub fn new(text: &str) -> Self {
        Self(CompactString::new(text))
    }

    pub const fn from_static(text: &'static str) -> Self {
        Self(CompactString::const_new(text))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn push(&mut self, ch: char) {
        self.0.push(ch);
    }

    pub fn push_str(&mut self, text: &str) {
        self.0.push_str(text);
    }

    /// Replaces the contents, reusing any heap buffer already held.
    pub fn set(&mut self, text: &str) {
        self.0.clear();
        self.0.push_str(text);
    }
}

impl Deref for CellSymbol {
    type Target = str;

    fn deref(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for CellSymbol {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl Borrow<str> for CellSymbol {
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for CellSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.0.as_str(), f)
    }
}

impl fmt::Display for CellSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl From<&str> for CellSymbol {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for CellSymbol {
    fn from(text: String) -> Self {
        Self(CompactString::from(text))
    }
}

impl From<&String> for CellSymbol {
    fn from(text: &String) -> Self {
        Self::new(text)
    }
}

impl From<char> for CellSymbol {
    fn from(ch: char) -> Self {
        Self(CompactString::new(ch.encode_utf8(&mut [0; 4])))
    }
}

impl From<CellSymbol> for String {
    fn from(symbol: CellSymbol) -> Self {
        symbol.0.into_string()
    }
}

impl PartialEq<str> for CellSymbol {
    fn eq(&self, other: &str) -> bool {
        self.0.as_str() == other
    }
}

impl PartialEq<&str> for CellSymbol {
    fn eq(&self, other: &&str) -> bool {
        self.0.as_str() == *other
    }
}

impl PartialEq<String> for CellSymbol {
    fn eq(&self, other: &String) -> bool {
        self.0.as_str() == other.as_str()
    }
}

impl PartialEq<CellSymbol> for str {
    fn eq(&self, other: &CellSymbol) -> bool {
        self == other.0.as_str()
    }
}

impl PartialEq<CellSymbol> for &str {
    fn eq(&self, other: &CellSymbol) -> bool {
        *self == other.0.as_str()
    }
}

impl Serialize for CellSymbol {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for CellSymbol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = CellSymbol;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<CellSymbol, E> {
                Ok(CellSymbol::new(text))
            }

            fn visit_string<E: serde::de::Error>(self, text: String) -> Result<CellSymbol, E> {
                Ok(CellSymbol::from(text))
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

/// Mirrors bincode's `String` decoding (length, container claim, UTF-8 check)
/// without the intermediate `Vec` for symbols that fit inline.
impl<Context> bincode::Decode<Context> for CellSymbol {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        let len = u64::decode(decoder)?;
        let len = usize::try_from(len).map_err(|_| DecodeError::OutsideUsizeRange(len))?;
        decoder.claim_container_read::<u8>(len)?;
        if len <= INLINE_CAPACITY {
            let mut buffer = [0u8; INLINE_CAPACITY];
            decoder.reader().read(&mut buffer[..len])?;
            let text =
                std::str::from_utf8(&buffer[..len]).map_err(|inner| DecodeError::Utf8 { inner })?;
            return Ok(Self::new(text));
        }
        let mut bytes = vec![0u8; len];
        decoder.reader().read(&mut bytes)?;
        String::from_utf8(bytes)
            .map(Self::from)
            .map_err(|err| DecodeError::Utf8 {
                inner: err.utf8_error(),
            })
    }
}
bincode::impl_borrow_decode!(CellSymbol);

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<String> {
        vec![
            String::new(),
            " ".to_owned(),
            "a".to_owned(),
            "⠋".to_owned(),
            "👩‍💻".to_owned(),
            "e\u{301}".to_owned(),
            "x".repeat(INLINE_CAPACITY),
            "y".repeat(INLINE_CAPACITY + 1),
            "─".repeat(40),
        ]
    }

    #[test]
    fn serde_encoding_matches_string() {
        let config = bincode::config::standard();
        for text in samples() {
            let symbol = CellSymbol::from(text.as_str());
            let expected = bincode::serde::encode_to_vec(&text, config).unwrap();
            let actual = bincode::serde::encode_to_vec(&symbol, config).unwrap();
            assert_eq!(actual, expected, "{text:?}");
            let (decoded, consumed): (CellSymbol, usize) =
                bincode::serde::decode_from_slice(&expected, config).unwrap();
            assert_eq!(decoded, text);
            assert_eq!(consumed, expected.len());
        }
    }

    #[test]
    fn native_decode_matches_string() {
        let config = bincode::config::standard();
        for text in samples() {
            let encoded = bincode::serde::encode_to_vec(&text, config).unwrap();
            let (decoded, consumed): (CellSymbol, usize) =
                bincode::decode_from_slice(&encoded, config).unwrap();
            assert_eq!(decoded, text);
            assert_eq!(consumed, encoded.len());
        }
    }

    #[test]
    fn native_decode_rejects_what_string_rejects() {
        let config = bincode::config::standard();
        for encoded in [vec![2u8, 0xff, 0xfe], vec![3u8, b'a'], vec![30u8; 31]] {
            let expected = bincode::decode_from_slice::<String, _>(&encoded, config).is_err();
            let actual = bincode::decode_from_slice::<CellSymbol, _>(&encoded, config).is_err();
            assert_eq!(actual, expected, "{encoded:?}");
        }
    }
}
