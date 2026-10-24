//! Konsolennamen: `[A-Za-z0-9_-]{1,32}`. Sie werden Teil eines Socket-Pfads.

use anyhow::{Result, bail};

pub const MAX_LEN: usize = 32;

pub fn validate(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_LEN {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("Name muss 1–{MAX_LEN} Zeichen lang sein: {name:?}")
            } else {
                format!("Name must be 1–{MAX_LEN} characters long: {name:?}")
            }
        );
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("Name darf nur A–Z, a–z, 0–9, _ und - enthalten: {name:?}")
            } else {
                format!("Name may only contain A–Z, a–z, 0–9, _ and -: {name:?}")
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for n in ["A", "b2", "my-console_1", &"x".repeat(MAX_LEN)] {
            assert!(validate(n).is_ok(), "{n}");
        }
    }

    #[test]
    fn invalid_names() {
        for n in ["", "../x", "a/b", "a b", "ä", ".", "a\0", &"x".repeat(MAX_LEN + 1)] {
            assert!(validate(n).is_err(), "{n:?}");
        }
    }
}
