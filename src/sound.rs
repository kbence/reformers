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
    tone[a..b].to_vec()
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
            (l[0] - l[n - 1]).abs() < 0.25,
            "loop seam {} → {}",
            l[n - 1],
            l[0]
        );
    }

    #[test]
    fn thud_dies_away() {
        let t = thud();
        let n = t.len();
        assert!(rms(&t[..n / 10]) > 0.1);
        assert!(rms(&t[9 * n / 10..]) < 0.1 * rms(&t[..n / 10]));
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
