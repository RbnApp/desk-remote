//! Annex-B helpers shared by every backend.

/// Offsets of the first byte after each Annex-B start code.
pub fn nal_starts(stream: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
}

/// `avc1.PPCCLL` derived from the first SPS in an Annex-B access unit.
pub fn avc1_codec(annex_b: &[u8]) -> Option<String> {
    nal_starts(annex_b).iter().find_map(|&s| {
        let nal = annex_b.get(s..)?;
        (nal.first()? & 0x1f == 7).then(|| {
            format!(
                "avc1.{:02X}{:02X}{:02X}",
                nal.get(1).copied().unwrap_or(0),
                nal.get(2).copied().unwrap_or(0),
                nal.get(3).copied().unwrap_or(0)
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_comes_from_the_sps() {
        let unit = [
            0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1F, 0xAA, 0, 0, 1, 0x65, 0x88,
        ];
        assert_eq!(avc1_codec(&unit).as_deref(), Some("avc1.42C01F"));
        assert!(avc1_codec(&[0, 0, 1, 0x65, 1]).is_none());
    }
}
