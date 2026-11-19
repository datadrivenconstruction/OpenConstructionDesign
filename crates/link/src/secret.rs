// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::fmt;

/// A value that must not reach a file, a log or a message: link code, project token, AI key.
/// It prints as `***`, cannot be serialized, and is wiped from memory when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The value itself: for a request header or the key-store call, nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.iter_mut().for_each(|b| *b = 0);
        // keeps the zeroing from being optimised away before the buffer is freed
        std::hint::black_box(&bytes);
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints() {
        let s = Secret::new("ddcl_MARKER");
        assert_eq!(format!("{s:?}"), "***");
        assert_eq!(format!("{:?}", Some(&s)), "Some(***)");
        assert_eq!(s.expose(), "ddcl_MARKER");
    }
}
