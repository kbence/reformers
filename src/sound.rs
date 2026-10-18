//! Sound, synthesised at start-up (no sample files). The original drives an AdLib (OPL2) chip
//! with two FM patches (docs/re-notes.md, Sound): a sustained engine tone (`[0xecb8]`, retuned
//! while driving) and a landing thud (`[0xecd9]`). This renders those patches with a small
//! two-operator FM voice, and adds skid noise and checkpoint chimes, as 44.1 kHz mono WAVs.

use std::f32::consts::{PI, TAU};

pub const RATE: u32 = 44_100;

/// The game's patch records (0x589b): modulator 0x20/0x40/0x60/0x80/0xE0, carrier the same, 0xC0.
pub const ENGINE_PATCH: [u8; 11] = [
    0x20, 0x00, 0xfb, 0x05, 0x00, 0x20, 0x00, 0xfb, 0x05, 0x00, 0x0a,
];
pub const THUD_PATCH: [u8; 11] = [
    0x00, 0x00, 0xf7, 0xa8, 0x00, 0x00, 0x00, 0xf3, 0x6a, 0x00, 0x02,
];

/// OPL2 note frequency from F-number and block.
pub fn opl_hz(fnum: u16, block: u8) -> f32 {
    fnum as f32 * 49_716.0 / (1u32 << (20 - block)) as f32
}

/// The engine's key-on note (0x5811: A1 = 0x00, B1 = 0x2f).
pub fn engine_idle_hz() -> f32 {
    opl_hz(0x300, 3)
}

/// The original's engine note for `revs` (`SimCar::revs`): `bx = |revs| + 0x1030` written raw
/// as A0/B0, i.e. block 4, F-number 0x30 + |revs| (capped where it would run into the block bits).
pub fn engine_fnum(revs: i32) -> u16 {
    (0x30 + revs.unsigned_abs().min(0x3ff - 0x30)) as u16
}

pub fn engine_note_hz(fnum: u16) -> f32 {
    opl_hz(fnum, 4)
}

/// The original's engine loudness for the throttle control word: carrier TL =
/// 0x3F − min(0x3F, 0x2C + |throttle >> 9|), 0.75 dB per step: −14 dB idle, −2 dB flat out.
pub fn engine_gain(throttle_word: i16) -> f32 {
    let tl = 0x3f - (0x2c + (throttle_word >> 9).unsigned_abs() as i32).min(0x3f);
    10f32.powf(-0.75 * tl as f32 / 20.0)
}

/// The thud's note (record bytes 11–12: A8 = 0x80, B8 = 0x21 → block 0, ~18 Hz).
/// ponytail: played 3 blocks (octaves) up, else it's a 9 Hz rumble laptop speakers can't make.
pub fn thud_hz() -> f32 {
    opl_hz(0x180, 3)
}

struct Op {
    mult: f32,
    gain: f32,
    sustains: bool,
    attack: f32,  // seconds to full
    decay: f32,   // seconds per 96 dB
    sustain: f32, // dB (≤ 0)
    release: f32, // seconds per 96 dB
}

/// OPL envelope rate → seconds for the full swing (attack 0→max, decay/release 96 dB);
/// rate 1 is slowest, each step about halves it, 15 is near-instant. 0 never moves.
fn rate_time(rate: u8, slowest: f32) -> f32 {
    match rate {
        0 => f32::INFINITY,
        15 => 0.0,
        r => slowest / (1u32 << (r - 1)) as f32,
    }
}

impl Op {
    fn from_regs(r: &[u8]) -> Op {
        const MULT: [f32; 16] = [
            0.5, 1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 10., 12., 12., 15., 15.,
        ];
        Op {
            mult: MULT[(r[0] & 15) as usize],
            gain: 10f32.powf(-0.75 * (r[1] & 63) as f32 / 20.0),
            sustains: r[0] & 0x20 != 0,
            attack: rate_time(r[2] >> 4, 2.826),
            decay: rate_time(r[2] & 15, 39.28),
            sustain: -3.0 * (r[3] >> 4) as f32 * if r[3] >> 4 == 15 { 31.0 / 15.0 } else { 1.0 },
            release: rate_time(r[3] & 15, 39.28),
        }
    }

    /// Envelope gain `t` seconds after key-on, keyed for `held` seconds.
    fn env(&self, t: f32, held: f32) -> f32 {
        if t < self.attack {
            return t / self.attack;
        }
        let db_at = |t: f32| {
            let d = t - self.attack;
            let to_sustain = -self.sustain / 96.0 * self.decay;
            if d < to_sustain {
                -96.0 * d / self.decay
            } else if self.sustains {
                self.sustain
            } else {
                // percussive: past the sustain level it keeps falling at the release rate
                self.sustain - 96.0 * (d - to_sustain) / self.release
            }
        };
        let db = if t < held {
            db_at(t)
        } else {
            db_at(held) - 96.0 * (t - held) / self.release
        };
        if db < -96.0 {
            0.0
        } else {
            10f32.powf(db / 20.0)
        }
    }
}

/// Renders an 11-byte patch at `hz` for `secs`, key held for `held` seconds.
pub fn render_patch(patch: &[u8; 11], hz: f32, held: f32, secs: f32) -> Vec<f32> {
    let (m, c) = (Op::from_regs(&patch[0..5]), Op::from_regs(&patch[5..10]));
    let fb = (patch[10] >> 1) & 7;
    let fm = patch[10] & 1 == 0;
    let fb_amount = if fb == 0 {
        0.0
    } else {
        PI / 16.0 * (1 << (fb - 1)) as f32
    };
    let (mut pm, mut pc, mut prev) = (0.0f32, 0.0f32, [0.0f32; 2]);
    (0..(secs * RATE as f32) as usize)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let mo =
                (TAU * pm + fb_amount * (prev[0] + prev[1]) / 2.0).sin() * m.gain * m.env(t, held);
            prev = [prev[1], mo];
            let co = if fm {
                (TAU * pc + TAU * mo).sin()
            } else {
                ((TAU * pc).sin() + mo) / 2.0
            } * c.gain
                * c.env(t, held);
            pm = (pm + hz * m.mult / RATE as f32).fract();
            pc = (pc + hz * c.mult / RATE as f32).fract();
            co
        })
        .collect()
}

/// A seamless loop of the sustained engine tone at `hz`: whole periods cut from its steady part.
pub fn engine_loop(hz: f32) -> Vec<f32> {
    let tone = render_patch(&ENGINE_PATCH, hz, 10.0, 0.6);
    let period = RATE as f32 / (hz * 0.5); // both operators run at MULT 0.5
    let periods = (0.25 * RATE as f32 / period).round().max(1.0);
    let start = (0.2 * RATE as f32 / period).ceil() * period;
    let (a, b) = (
        start.round() as usize,
        (start + periods * period).round() as usize,
    );
    analog_out(&tone[a..b])
}

/// The card's analogue output stage, roughly: two one-pole low-passes at ~3.5 kHz. The patch's
/// feedback makes a sawtooth-like wave with a hard reset every cycle; raw, that's a full-scale
/// jump between two samples, heard as a click per cycle (a Geiger counter at idle). Run round
/// the loop twice so the filter has settled at the seam.
fn analog_out(x: &[f32]) -> Vec<f32> {
    let k = 1.0 - (-TAU * 3500.0 / RATE as f32).exp();
    let (mut a, mut b) = (0.0f32, 0.0f32);
    let mut out = vec![0.0; x.len()];
    for pass in 0..2 {
        for (i, &v) in x.iter().enumerate() {
            a += (v - a) * k;
            b += (a - b) * k;
            if pass == 1 {
                out[i] = b;
            }
        }
    }
    out
}

pub fn thud() -> Vec<f32> {
    render_patch(&THUD_PATCH, thud_hz(), 0.05, 0.6)
}

/// Low-passed noise, loopable (the ends fade into each other).
pub fn skid_loop() -> Vec<f32> {
    let n = RATE as usize / 2;
    let mut seed = 0x1234_5678u32;
    let mut lp = 0.0f32;
    let noise: Vec<f32> = (0..n)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            lp += 0.15 * ((seed as f32 / u32::MAX as f32) * 2.0 - 1.0 - lp);
            lp * 2.0
        })
        .collect();
    // crossfade the last tenth into the start so the loop point is inaudible
    let fade = n / 10;
    (0..n - fade)
        .map(|i| {
            if i < fade {
                let w = i as f32 / fade as f32;
                noise[i] * w + noise[n - fade + i] * (1.0 - w)
            } else {
                noise[i]
            }
        })
        .collect()
}

/// Decaying sine notes, one after another (`(hz, seconds)`).
pub fn chime(notes: &[(f32, f32)]) -> Vec<f32> {
    notes
        .iter()
        .flat_map(|&(hz, secs)| {
            (0..(secs * RATE as f32) as usize).map(move |i| {
                let t = i as f32 / RATE as f32;
                (TAU * hz * t).sin() * (-t * 6.0).exp() * (t * 400.0).min(1.0) * 0.5
            })
        })
        .collect()
}

/// Samples (−1…1, channels mixed to mono) of an uncompressed 16-bit PCM WAV; skips other chunks
/// (LIST metadata etc.). None for anything else.
pub fn read_wav(d: &[u8]) -> Option<Vec<f32>> {
    if d.get(..4)? != b"RIFF" || d.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut at, mut channels) = (12, 0usize);
    while at + 8 <= d.len() {
        let len = u32::from_le_bytes(d[at + 4..at + 8].try_into().ok()?) as usize;
        let body = d.get(at + 8..(at + 8 + len).min(d.len()))?;
        match &d[at..at + 4] {
            b"fmt " => {
                let format = u16::from_le_bytes([body[0], body[1]]);
                let bits = u16::from_le_bytes([body[14], body[15]]);
                if format != 1 || bits != 16 {
                    return None;
                }
                channels = u16::from_le_bytes([body[2], body[3]]) as usize;
            }
            b"data" if channels > 0 => {
                return Some(
                    body.chunks_exact(2 * channels)
                        .map(|f| {
                            f.as_chunks::<2>()
                                .0
                                .iter()
                                .map(|&b| i16::from_le_bytes(b) as f32 / 32768.0)
                                .sum::<f32>()
                                / channels as f32
                        })
                        .collect(),
                );
            }
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    None
}

/// A loop played `factor` times faster (higher pitch, shorter), by linear interpolation;
/// wraps around at the end so it still loops.
pub fn speed_up(x: &[f32], factor: f32) -> Vec<f32> {
    let n = ((x.len() as f32 / factor) as usize).max(1);
    (0..n)
        .map(|i| {
            let p = i as f32 * x.len() as f32 / n as f32;
            let (j, t) = (p as usize % x.len(), p.fract());
            x[j] * (1.0 - t) + x[(j + 1) % x.len()] * t
        })
        .collect()
}

/// 16-bit mono PCM WAV.
pub fn wav(samples: &[f32]) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend(b"RIFF");
    out.extend((36 + data).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data.to_le_bytes());
    out.extend(
        samples
            .iter()
            .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    #[test]
    fn opl_notes() {
        assert!(
            (engine_idle_hz() - 291.3).abs() < 0.5,
            "{}",
            engine_idle_hz()
        );
        assert!((opl_hz(0x180, 0) - 18.2).abs() < 0.1);
    }

    #[test]
    fn original_engine_mapping() {
        assert_eq!(engine_fnum(0), 0x30);
        assert_eq!(engine_fnum(-100), 0x30 + 100);
        assert_eq!(engine_fnum(5000), 0x3ff, "capped before the block bits");
        assert!(
            (engine_note_hz(0x30) - 36.4).abs() < 0.1,
            "18 Hz after MULT ½"
        );
        assert!((20.0 * engine_gain(0).log10() + 14.25).abs() < 0.01);
        assert!((20.0 * engine_gain(0x2000).log10() + 2.25).abs() < 0.01);
        assert_eq!(engine_gain(-0x2000), engine_gain(0x2000));
    }

    #[test]
    fn engine_is_steady_and_loops() {
        let l = engine_loop(engine_idle_hz());
        let n = l.len();
        assert!(n > RATE as usize / 5, "about a quarter second");
        let (head, tail) = (rms(&l[..n / 4]), rms(&l[3 * n / 4..]));
        assert!(
            head > 0.2 && (head - tail).abs() < 0.05 * head,
            "sustained: {head} vs {tail}"
        );
        assert!(l.iter().all(|s| s.abs() <= 1.0));
        assert!(
            (l[0] - l[n - 1]).abs() < 0.05,
            "loop seam {} → {}",
            l[n - 1],
            l[0]
        );
        // no click-sized jumps between samples (the raw FM tone has full-scale ones)
        for hz in [engine_note_hz(48), engine_note_hz(300)] {
            let t = engine_loop(hz);
            let step = t
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max);
            assert!(step < 0.5, "largest step {step} at {hz} Hz");
        }
    }

    #[test]
    fn thud_dies_away() {
        let t = thud();
        let n = t.len();
        assert!(rms(&t[..n / 10]) > 0.1);
        assert!(rms(&t[9 * n / 10..]) < 0.1 * rms(&t[..n / 10]));
    }

    #[test]
    fn reads_wav_back_and_speeds_up() {
        let tone: Vec<f32> = (0..4410)
            .map(|i| (TAU * 100.0 * i as f32 / RATE as f32).sin() * 0.5)
            .collect();
        let back = read_wav(&wav(&tone)).unwrap();
        assert_eq!(back.len(), tone.len());
        assert!(back.iter().zip(&tone).all(|(a, b)| (a - b).abs() < 1e-3));
        // a LIST chunk before the data, like the recorded engine loops, is skipped
        let mut with_list = wav(&tone);
        let list = [b"LIST".as_slice(), &4u32.to_le_bytes(), b"INFO"].concat();
        with_list.splice(36..36, list);
        assert_eq!(read_wav(&with_list).unwrap().len(), tone.len());
        let fast = speed_up(&tone, 2.0);
        assert_eq!(fast.len(), 2205);
        let crossings = |x: &[f32]| x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert_eq!(
            crossings(&fast),
            crossings(&tone),
            "same cycles in half the time: double pitch"
        );
        assert!(read_wav(b"RIFF....WAVEjunk").is_none());
    }

    #[test]
    fn reads_the_recorded_engine() {
        crate::need_assets!();
        let b = crate::assets::read("sounds/engine/loop_0.wav").unwrap();
        let s = read_wav(&b).expect("loop_0.wav parses");
        assert_eq!(s.len(), 38106);
    }

    #[test]
    fn wav_header() {
        let w = wav(&[0.0, 1.0, -1.0]);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
        assert_eq!(&w[44..], &[0, 0, 0xff, 0x7f, 0x01, 0x80]);
        assert!(rms(&skid_loop()) > 0.05 && !chime(&[(880.0, 0.2)]).is_empty());
    }
}
