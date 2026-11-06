//! BOOKOO's published Mini Scale protocol:
//! <https://github.com/BooKooCode/OpenSource/blob/main/bookoo_mini_scale/protocols.md>

pub(crate) const SERVICE: &str = "00000ffe-0000-1000-8000-00805f9b34fb";
pub(crate) const WEIGHT: &str = "0000ff11-0000-1000-8000-00805f9b34fb";
pub(crate) const COMMAND: &str = "0000ff12-0000-1000-8000-00805f9b34fb";
pub(crate) const TARE: [u8; 6] = [0x03, 0x0a, 0x01, 0, 0, 0x08];

/// Complete, checked weight notifications only. The scale's timer is a
/// resettable brew timer, so elapsed time is measured by the receiver.
pub(crate) fn grams(packet: &[u8]) -> Option<f64> {
    if packet.len() != 20
        || packet[..2] != [0x03, 0x0b]
        || packet.iter().fold(0, |sum, b| sum ^ b) != 0
    {
        return None;
    }
    let sign = match packet[6] {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let hundredths = u32::from_be_bytes([0, packet[7], packet[8], packet[9]]);
    // The Mini's protocol only reports grams. Reject values outside its
    // 2 kg capacity, including corrupt overload readings.
    (hundredths <= 200_000).then_some(sign * f64::from(hundredths) / 100.0)
}

#[cfg(test)]
pub(crate) fn frame(grams: f64) -> Vec<u8> {
    let mut p = vec![0; 20];
    p[..2].copy_from_slice(&[3, 11]);
    p[6] = if grams < 0.0 { b'-' } else { b'+' };
    let bytes = ((grams.abs() * 100.0).round() as u32).to_be_bytes();
    p[7..10].copy_from_slice(&bytes[1..]);
    p[19] = p[..19].iter().fold(0, |sum, b| sum ^ b);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_signed_big_endian_weights_and_rejects_bad_frames() {
        let known = [
            3, 11, 0, 0, 0, 0, b'+', 0, 15, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 46,
        ];
        assert_eq!(grams(&known), Some(38.42));
        for g in [0.0, 38.42, -150.0, 2000.0] {
            assert_eq!(grams(&frame(g)), Some(g));
        }
        let p = frame(38.42);
        for n in 0..20 {
            assert_eq!(grams(&p[..n]), None);
        }
        let mut p = p;
        p[8] ^= 1;
        assert_eq!(grams(&p), None);
        assert_eq!(grams(&frame(2000.01)), None);
        assert_eq!(TARE.iter().fold(0, |sum, b| sum ^ b), 0);
    }
}
