//! Conversion between PipeWire's volume fraction and the receiver's master volume.
//!
//! The fraction is the receiver's `MV` number divided by 100, so PipeWire's percentage reads
//! the same as the receiver's display. `MV` is already linear in dB (MV 80 is 0 dB, in
//! half-dB steps), which is why PipeWire's usual cubic curve isn't applied: its software
//! volume stays at 100% for this device, so the curve would only distort the slider.

use std::fmt;

/// A master-volume level counted in half steps: `MV62` is 124 and `MV605` is 121.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mv(u16);

impl Mv {
    pub const ZERO: Mv = Mv(0);

    pub const fn from_half_steps(half_steps: u16) -> Self {
        Mv(half_steps)
    }

    pub const fn from_whole(whole: u16) -> Self {
        Mv(whole * 2)
    }

    pub const fn half_steps(self) -> u16 {
        self.0
    }

    /// Parses the digits after `MV`: two digits, optionally followed by `5` for a half step.
    pub fn parse(digits: &str) -> Option<Self> {
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        match digits.len() {
            2 => digits.parse().ok().map(Mv::from_whole),
            3 if digits.ends_with('5') => digits[..2].parse::<u16>().ok().map(|w| Mv(w * 2 + 1)),
            _ => None,
        }
    }
}

impl fmt::Display for Mv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}", self.0 / 2)?;
        if self.0 % 2 == 1 {
            f.write_str("5")?;
        }
        Ok(())
    }
}

/// Maps a PipeWire fraction to the nearest half step, never above `cap`.
pub fn to_mv(v: f64, cap: Mv) -> Mv {
    if v.is_nan() || v <= 0.0 {
        return Mv::ZERO;
    }
    let half_steps = (v * 200.0).round().clamp(0.0, f64::from(cap.0));
    Mv(half_steps as u16)
}

pub fn from_mv(mv: Mv) -> f64 {
    f64::from(mv.0) / 200.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Mv = Mv::from_whole(70);

    #[test]
    fn parses_wire_forms() {
        assert_eq!(Mv::parse("62"), Some(Mv::from_half_steps(124)));
        assert_eq!(Mv::parse("605"), Some(Mv::from_half_steps(121)));
        assert_eq!(Mv::parse("00"), Some(Mv::ZERO));
        assert_eq!(Mv::parse("005"), Some(Mv::from_half_steps(1)));
        assert_eq!(Mv::parse("6"), None);
        assert_eq!(Mv::parse("604"), None);
        assert_eq!(Mv::parse("MAX 98"), None);
    }

    #[test]
    fn formats_wire_forms() {
        assert_eq!(Mv::from_half_steps(124).to_string(), "62");
        assert_eq!(Mv::from_half_steps(121).to_string(), "605");
        assert_eq!(Mv::ZERO.to_string(), "00");
        assert_eq!(Mv::from_half_steps(1).to_string(), "005");
    }

    #[test]
    fn percent_is_the_display_number() {
        assert_eq!(from_mv(Mv::from_whole(62)), 0.62);
        assert_eq!(from_mv(Mv::from_half_steps(121)), 0.605);
        assert_eq!(to_mv(0.62, CAP), Mv::from_whole(62));
        assert_eq!(to_mv(0.613, CAP), Mv::from_half_steps(123));
    }

    #[test]
    fn writes_stop_at_the_cap() {
        assert_eq!(to_mv(0.70, CAP), CAP);
        assert_eq!(to_mv(0.90, CAP), CAP);
        assert_eq!(to_mv(1.5, CAP), CAP);
    }

    #[test]
    fn silence_and_garbage_map_to_zero() {
        assert_eq!(to_mv(0.0, CAP), Mv::ZERO);
        assert_eq!(to_mv(-1.0, CAP), Mv::ZERO);
        assert_eq!(to_mv(f64::NAN, CAP), Mv::ZERO);
        assert_eq!(to_mv(1e-6, CAP), Mv::ZERO);
        assert_eq!(from_mv(Mv::ZERO), 0.0);
    }

    #[test]
    fn every_step_round_trips() {
        for half_steps in 1..=CAP.half_steps() {
            let mv = Mv::from_half_steps(half_steps);
            assert_eq!(to_mv(from_mv(mv), CAP), mv, "{mv}");
        }
    }

    #[test]
    fn levels_above_the_cap_read_as_they_are() {
        assert_eq!(from_mv(Mv::from_whole(80)), 0.80);
    }
}
