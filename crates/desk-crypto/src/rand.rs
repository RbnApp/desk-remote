//! Operating-system CSPRNG access and hex helpers.
//!
//! Every secret in Desk-Remote (pairing code, device keys, salts) comes from the OS
//! entropy source through `getrandom`. No userland PRNG is used for secrets.

use desk_error::{DeskError, DeskResult};

/// Fills `buf` with cryptographically secure random bytes from the OS.
pub fn fill_random(buf: &mut [u8]) -> DeskResult<()> {
    getrandom::getrandom(buf)
        .map_err(|e| DeskError::Crypto(format!("OS random source unavailable: {e}")))
}

/// Returns a uniformly distributed index in `0..bound` (rejection sampling, no modulo bias).
pub fn random_index(bound: usize) -> DeskResult<usize> {
    if bound == 0 || bound > 256 {
        return Err(DeskError::Crypto(format!("invalid random bound {bound}")));
    }
    // Largest multiple of `bound` within the byte range; larger samples are rejected.
    let limit = 256 - (256 % bound);
    loop {
        let mut b = [0u8; 1];
        fill_random(&mut b)?;
        let v = usize::from(b[0]);
        if v < limit {
            return Ok(v % bound);
        }
    }
}

/// Lower-case hex encoding.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Writing to a String cannot fail.
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Decodes a hex string of exactly `N` bytes.
pub fn from_hex<const N: usize>(s: &str) -> DeskResult<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return Err(DeskError::Crypto("invalid hex length".to_string()));
    }
    let mut out = [0u8; N];
    for (i, item) in out.iter_mut().enumerate() {
        *item = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| DeskError::Crypto(format!("invalid hex character: {e}")))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_bytes_differ_and_are_nonzero() -> DeskResult<()> {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        fill_random(&mut a)?;
        fill_random(&mut b)?;
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
        Ok(())
    }

    #[test]
    fn random_index_stays_in_range() -> DeskResult<()> {
        for _ in 0..500 {
            assert!(random_index(24)? < 24);
        }
        Ok(())
    }

    #[test]
    fn hex_roundtrip() -> DeskResult<()> {
        let bytes = [1u8, 0xab, 0xff, 0];
        assert_eq!(from_hex::<4>(&to_hex(&bytes))?, bytes);
        assert!(from_hex::<4>("zz").is_err());
        Ok(())
    }
}
