//! SMPTE LTC (linear timecode) "warble" generator -- a native-audio port
//! of `web/index.html`'s `LTC_WORKLET_SOURCE`. Keep the two in sync if
//! either changes; see that file's module doc comment for the full
//! bit-level frame format (confirmed against SMPTE 12M's published
//! layout, then corrected against a real decode -- seconds-tens and
//! minutes-tens are 3-bit fields, not 2-bit ones, which can only
//! represent 0-3, not the 0-5 a tens digit needs. That bug produced
//! completely plausible output for any tens digit 0-3 and only
//! surfaced on a test that happened to cross a :58/:59 boundary).
//!
//! This module is deliberately independent of any audio I/O library
//! (`cpal` in main.rs's UI code is the only place that touches real
//! hardware) so its correctness can be tested in complete isolation,
//! the same reasoning as keeping `video.rs`'s frame-sampling logic
//! separate from decode_qr's caller.

/// Builds the 64 BCD/flag/user-bit bits plus the 16-bit sync word for
/// one LTC frame (bits 0-79; see the web version's doc comment for the
/// full field table).
fn build_frame_bits(h: u8, m: u8, s: u8, f: u8, drop_frame: bool) -> [u8; 80] {
    let mut bits = [0u8; 80];
    set_bcd(&mut bits, 0, 4, f % 10);
    set_bcd(&mut bits, 8, 2, f / 10);
    bits[10] = u8::from(drop_frame);
    set_bcd(&mut bits, 16, 4, s % 10);
    set_bcd(&mut bits, 24, 3, s / 10);
    set_bcd(&mut bits, 32, 4, m % 10);
    set_bcd(&mut bits, 40, 3, m / 10);
    set_bcd(&mut bits, 48, 4, h % 10);
    set_bcd(&mut bits, 56, 2, h / 10);
    const SYNC: [u8; 16] = [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1];
    bits[64..80].copy_from_slice(&SYNC);
    bits
}

fn set_bcd(bits: &mut [u8; 80], start: usize, width: usize, value: u8) {
    for i in 0..width {
        bits[start + i] = (value >> i) & 1;
    }
}

/// Drop-frame skips displayed frame *numbers* 0 and 1 at the start of
/// every minute except ones divisible by 10. Only ever nudges `f` from
/// 0/1 up to 2 -- a no-op for every other `f` -- so it's safe to call
/// unconditionally right after `f` is set to 0 at a minute boundary.
pub(crate) fn first_valid_frame(f: u8, m: u8, drop_frame: bool) -> u8 {
    if drop_frame && m % 10 != 0 && f < 2 {
        2
    } else {
        f
    }
}

/// Live H:M:S:F readout, shared between the audio callback thread (which
/// writes it once per generated frame) and the UI thread (which reads it
/// once per repaint) -- see main.rs's `ltc_readout` field.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Timecode {
    pub h: u8,
    pub m: u8,
    pub s: u8,
    pub f: u8,
}

/// Stateful BMC (biphase mark coding) sample generator: call
/// [`Self::next_sample`] once per output sample, continuously, from
/// whatever's producing real audio (a `cpal` stream callback in
/// main.rs). Advances its own H:M:S:F; read it back via
/// [`Self::timecode`] rather than tracking it separately.
pub struct LtcGenerator {
    frame_bits: [u8; 80],
    bit_index: usize,
    in_second_half: bool,
    polarity: f32,
    samples_per_bit: f64,
    samples_left_in_segment: f64,
    nominal_frame_count: u8,
    drop_frame: bool,
    h: u8,
    m: u8,
    s: u8,
    f: u8,
}

impl LtcGenerator {
    /// `fps` is the nominal frame rate (24.0, 25.0, 29.97, or 30.0);
    /// `(h, m, s, f)` is the starting timecode -- typically read from the
    /// wall clock at the moment generation starts (jam-sync), with `f`
    /// already passed through [`first_valid_frame`] if `drop_frame`.
    pub fn new(sample_rate: f64, fps: f64, drop_frame: bool, h: u8, m: u8, s: u8, f: u8) -> Self {
        let nominal_frame_count = fps.round() as u8;
        let samples_per_bit = sample_rate / (fps * 80.0);
        let frame_bits = build_frame_bits(h, m, s, f, drop_frame);
        let mut generator = Self {
            frame_bits,
            bit_index: 0,
            in_second_half: false,
            polarity: 1.0,
            samples_per_bit,
            samples_left_in_segment: 0.0,
            nominal_frame_count,
            drop_frame,
            h,
            m,
            s,
            f,
        };
        generator.samples_left_in_segment = generator.segment_duration();
        generator
    }

    pub fn timecode(&self) -> Timecode {
        Timecode {
            h: self.h,
            m: self.m,
            s: self.s,
            f: self.f,
        }
    }

    /// A '1' bit is two half-length segments (an extra transition at the
    /// cell's midpoint); a '0' bit is one full-length segment.
    fn segment_duration(&self) -> f64 {
        if self.frame_bits[self.bit_index] == 1 {
            self.samples_per_bit / 2.0
        } else {
            self.samples_per_bit
        }
    }

    fn advance(&mut self) {
        let bit = self.frame_bits[self.bit_index];
        if bit == 1 && !self.in_second_half {
            self.in_second_half = true;
        } else {
            self.in_second_half = false;
            self.bit_index += 1;
            if self.bit_index >= 80 {
                self.bit_index = 0;
                self.f += 1;
                if self.f >= self.nominal_frame_count {
                    self.f = 0;
                    self.s += 1;
                    if self.s >= 60 {
                        self.s = 0;
                        self.m += 1;
                        if self.m >= 60 {
                            self.m = 0;
                            self.h = (self.h + 1) % 24;
                        }
                        self.f = first_valid_frame(self.f, self.m, self.drop_frame);
                    }
                }
                self.frame_bits = build_frame_bits(self.h, self.m, self.s, self.f, self.drop_frame);
            }
        }
        // += (not =) carries over any fractional remainder from the
        // previous segment, so a non-integer samples-per-bit rate (e.g.
        // 29.97fps at 48kHz) stays accurate on average instead of
        // drifting -- the same idea as Bresenham's line algorithm.
        self.samples_left_in_segment += self.segment_duration();
        self.polarity = -self.polarity;
    }

    /// Returns the next bipolar sample (roughly [-1.0, 1.0]); call this
    /// once per audio sample, continuously, while the generator is
    /// active.
    pub fn next_sample(&mut self) -> f32 {
        let sample = self.polarity * 0.8;
        self.samples_left_in_segment -= 1.0;
        if self.samples_left_in_segment <= 0.0 {
            self.advance();
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_valid_frame_only_nudges_01_at_non_tens_minutes() {
        assert_eq!(first_valid_frame(0, 1, true), 2);
        assert_eq!(first_valid_frame(1, 1, true), 2);
        assert_eq!(first_valid_frame(5, 1, true), 5); // already >= 2, untouched
        assert_eq!(first_valid_frame(0, 10, true), 0); // tens minute, no skip
        assert_eq!(first_valid_frame(0, 1, false), 0); // non-drop, no skip ever
    }

    #[test]
    fn build_frame_bits_sync_word_is_always_present() {
        let bits = build_frame_bits(12, 34, 56, 7, false);
        assert_eq!(
            &bits[64..80],
            &[0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1]
        );
    }

    #[test]
    fn build_frame_bits_drop_frame_flag_bit_matches_the_argument() {
        assert_eq!(build_frame_bits(0, 0, 0, 0, false)[10], 0);
        assert_eq!(build_frame_bits(0, 0, 0, 0, true)[10], 1);
    }

    // ── independent decode validation ───────────────────────────────────
    //
    // A from-scratch biphase demodulator, reading transition timings out
    // of generated samples the way a real hardware LTC reader would --
    // not reusing LtcGenerator's own bit-stepping assumptions. This is
    // exactly the kind of test that caught the 2-bit-vs-3-bit tens-field
    // bug in the web version (see this module's doc comment): any test
    // using only a 0-3 tens digit would have looked completely fine.

    fn find_transitions(samples: &[f32]) -> Vec<usize> {
        let mut transitions = Vec::new();
        let mut prev_sign = samples[0] >= 0.0;
        for (i, &sample) in samples.iter().enumerate().skip(1) {
            let sign = sample >= 0.0;
            if sign != prev_sign {
                transitions.push(i);
                prev_sign = sign;
            }
        }
        transitions
    }

    fn gaps_to_bits(gaps: &[usize]) -> Vec<u8> {
        let mut sorted = gaps.to_vec();
        sorted.sort_unstable();
        let baseline = sorted[sorted.len() / 4] as f64;
        let threshold = baseline * 1.5;

        let mut bits = Vec::new();
        let mut i = 0;
        while i < gaps.len() {
            if gaps[i] as f64 > threshold {
                bits.push(0);
                i += 1;
            } else if i + 1 < gaps.len() && (gaps[i + 1] as f64) <= threshold {
                bits.push(1);
                i += 2;
            } else {
                i += 1;
            }
        }
        bits
    }

    const SYNC_WORD: [u8; 16] = [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1];

    fn bcd(bits: &[u8]) -> u8 {
        bits.iter()
            .enumerate()
            .fold(0u8, |acc, (i, &b)| acc | (b << i))
    }

    fn decode_frames(bits: &[u8]) -> Vec<(u8, u8, u8, u8, u8)> {
        let mut out = Vec::new();
        let mut last_start: Option<usize> = None;
        let mut start = 0;
        while start + 80 <= bits.len() {
            if bits[start + 64..start + 80] == SYNC_WORD
                && last_start.is_none_or(|prev| start - prev >= 70)
            {
                let fb = &bits[start..start + 64];
                let f = bcd(&fb[0..4]) + bcd(&fb[8..10]) * 10;
                let drop_frame_flag = fb[10];
                let s = bcd(&fb[16..20]) + bcd(&fb[24..27]) * 10;
                let m = bcd(&fb[32..36]) + bcd(&fb[40..43]) * 10;
                let h = bcd(&fb[48..52]) + bcd(&fb[56..58]) * 10;
                out.push((h, m, s, f, drop_frame_flag));
                last_start = Some(start);
            }
            start += 1;
        }
        out
    }

    fn generate_samples(generator: &mut LtcGenerator, seconds: f64, sample_rate: f64) -> Vec<f32> {
        let total = (sample_rate * seconds) as usize;
        (0..total).map(|_| generator.next_sample()).collect()
    }

    fn decode(samples: &[f32]) -> Vec<(u8, u8, u8, u8, u8)> {
        let transitions = find_transitions(samples);
        let gaps: Vec<usize> = transitions.windows(2).map(|w| w[1] - w[0]).collect();
        let bits = gaps_to_bits(&gaps);
        decode_frames(&bits)
    }

    /// Asserts a decoded frame sequence is internally consistent: frame
    /// increments by 1 within a second, resets to 0 (or jumps to 2, at a
    /// drop-frame non-tens-minute boundary) when the second advances by
    /// exactly 1, and the drop-frame flag bit matches throughout.
    fn assert_sequential(frames: &[(u8, u8, u8, u8, u8)], expect_drop_frame: bool) {
        assert!(
            frames.len() >= 2,
            "expected at least 2 decoded frames, got {}",
            frames.len()
        );
        for w in frames.windows(2) {
            let (ph, pm, ps, pf, pdf) = w[0];
            let (ch, cm, cs, cf, cdf) = w[1];
            assert_eq!(pdf, u8::from(expect_drop_frame), "prev frame {:?}", w[0]);
            assert_eq!(cdf, u8::from(expect_drop_frame), "cur frame {:?}", w[1]);
            let prev_total = (ph as u32 * 60 + pm as u32) * 60 + ps as u32;
            let cur_total = (ch as u32 * 60 + cm as u32) * 60 + cs as u32;
            if cur_total == prev_total {
                assert_eq!(
                    cf,
                    pf + 1,
                    "frame should increment within the same second: {:?} -> {:?}",
                    w[0],
                    w[1]
                );
            } else if cur_total == prev_total + 1 || (prev_total == 86399 && cur_total == 0) {
                let minute_changed = (ch as u32 * 60 + cm as u32) != (ph as u32 * 60 + pm as u32);
                if minute_changed && expect_drop_frame && cm % 10 != 0 {
                    assert_eq!(
                        cf, 2,
                        "drop-frame should skip 0/1 at non-tens minute {cm}: {:?} -> {:?}",
                        w[0], w[1]
                    );
                } else {
                    assert_eq!(
                        cf, 0,
                        "frame should reset to 0 on a new second: {:?} -> {:?}",
                        w[0], w[1]
                    );
                }
            } else {
                panic!(
                    "seconds did not advance by exactly 1: {:?} -> {:?}",
                    w[0], w[1]
                );
            }
        }
    }

    #[test]
    fn round_trip_all_fps_options_crossing_a_tens_digit_5_boundary() {
        // Starting at :58 specifically exercises the tens digit that
        // exposed the original 2-bit-field bug.
        for fps in [24.0, 25.0, 29.97, 30.0] {
            let mut generator = LtcGenerator::new(48_000.0, fps, false, 10, 4, 58, 0);
            let samples = generate_samples(&mut generator, 3.0, 48_000.0);
            let frames = decode(&samples);
            assert_sequential(&frames, false);
        }
    }

    #[test]
    fn round_trip_drop_frame_skips_at_a_non_tens_minute_boundary() {
        let mut generator = LtcGenerator::new(48_000.0, 29.97, true, 10, 4, 58, 0);
        let samples = generate_samples(&mut generator, 4.0, 48_000.0);
        let frames = decode(&samples);
        assert_sequential(&frames, true);
        // Confirm the skip actually happened somewhere in this run.
        assert!(
            frames
                .iter()
                .any(|&(h, m, s, f, _)| (h, m, s, f) == (10, 5, 0, 2)),
            "expected to see 10:05:00;02 (the post-skip frame) somewhere in {frames:?}"
        );
    }

    #[test]
    fn round_trip_drop_frame_does_not_skip_at_a_tens_minute_boundary() {
        let mut generator = LtcGenerator::new(48_000.0, 29.97, true, 10, 9, 58, 0);
        let samples = generate_samples(&mut generator, 4.0, 48_000.0);
        let frames = decode(&samples);
        assert_sequential(&frames, true);
        assert!(
            frames
                .iter()
                .any(|&(h, m, s, f, _)| (h, m, s, f) == (10, 10, 0, 0)),
            "expected to see 10:10:00;00 (unskipped) somewhere in {frames:?}"
        );
    }

    #[test]
    fn round_trip_hour_and_midnight_rollover() {
        let mut generator = LtcGenerator::new(48_000.0, 25.0, false, 23, 59, 59, 0);
        let samples = generate_samples(&mut generator, 2.0, 48_000.0);
        let frames = decode(&samples);
        assert!(
            frames.iter().any(|&(h, m, s, _, _)| (h, m, s) == (0, 0, 0)),
            "expected to see 00:00:00 after midnight rollover in {frames:?}"
        );
    }
}
