//! Annex B (start-code delimited) H.264, as Media Foundation encoders emit
//! it, versus the length-prefixed form MP4 stores.

const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

/// One encoded access unit, converted for MP4.
pub(crate) struct AccessUnit {
    /// Slice NAL units, each prefixed with its 4-byte big-endian length.
    pub avcc: Vec<u8>,
    pub keyframe: bool,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

/// Splits Annex B data into NAL units (without start codes). Trailing zero
/// bytes belong to the next 4-byte start code, never to a NAL unit, whose
/// last byte always holds the RBSP stop bit.
pub(crate) fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(n, &start)| {
            let end = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
            let mut nal = &data[start..end];
            while let [rest @ .., 0] = nal {
                nal = rest;
            }
            (!nal.is_empty()).then_some(nal)
        })
        .collect()
}

/// Converts one Annex B access unit for MP4. SPS/PPS move into the track
/// header (returned separately) and access unit delimiters are dropped.
pub(crate) fn to_access_unit(annex_b: &[u8]) -> AccessUnit {
    let mut unit = AccessUnit { avcc: Vec::with_capacity(annex_b.len()), keyframe: false, sps: None, pps: None };
    for nal in nal_units(annex_b) {
        match nal[0] & 0x1f {
            NAL_SPS => unit.sps = Some(nal.to_vec()),
            NAL_PPS => unit.pps = Some(nal.to_vec()),
            NAL_AUD => {}
            kind => {
                unit.keyframe |= kind == NAL_IDR;
                unit.avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                unit.avcc.extend_from_slice(nal);
            }
        }
    }
    unit
}

/// Appends one MP4 access unit (NAL units each prefixed with a 4-byte
/// big-endian length, as [`to_access_unit`] writes them) to `annex_b` with
/// start codes instead. `None` if a length runs past the end.
pub(crate) fn append_annex_b(avcc: &[u8], annex_b: &mut Vec<u8>) -> Option<()> {
    let mut rest = avcc;
    while let Some((length, after)) = rest.split_first_chunk::<4>() {
        let (nal, next) = after.split_at_checked(u32::from_be_bytes(*length) as usize)?;
        annex_b.extend_from_slice(&[0, 0, 0, 1]);
        annex_b.extend_from_slice(nal);
        rest = next;
    }
    rest.is_empty().then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mixed_start_codes_and_strips_trailing_zeros() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 0, 1, 0x65, 4, 5];
        let nals = nal_units(&data);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
    }

    #[test]
    fn keyframe_moves_parameter_sets_out_and_length_prefixes_slices() {
        let data = [0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x67, 0x64, 0, 0, 0, 1, 0x68, 0xee, 0, 0, 1, 0x65, 0xaa, 0xbb];
        let unit = to_access_unit(&data);
        assert!(unit.keyframe);
        assert_eq!(unit.sps.as_deref(), Some(&[0x67, 0x64][..]));
        assert_eq!(unit.pps.as_deref(), Some(&[0x68, 0xee][..]));
        assert_eq!(unit.avcc, vec![0, 0, 0, 3, 0x65, 0xaa, 0xbb]);
    }

    #[test]
    fn non_idr_slice_is_not_a_keyframe() {
        let unit = to_access_unit(&[0, 0, 1, 0x41, 0x9a]);
        assert!(!unit.keyframe);
        assert_eq!(unit.avcc, vec![0, 0, 0, 2, 0x41, 0x9a]);
    }

    #[test]
    fn length_prefixes_become_start_codes() {
        let mut annex_b = vec![9];
        assert!(append_annex_b(&[0, 0, 0, 2, 0x65, 0xaa, 0, 0, 0, 1, 0x41], &mut annex_b).is_some());
        assert_eq!(annex_b, vec![9, 0, 0, 0, 1, 0x65, 0xaa, 0, 0, 0, 1, 0x41]);
        assert!(append_annex_b(&[0, 0, 0, 3, 0x65], &mut annex_b).is_none());
        assert!(append_annex_b(&[0, 0], &mut annex_b).is_none());
    }
}
