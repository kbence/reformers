//! Best-lap ghost: the car's shape sampled every `STEP` ticks of a lap, replayed alongside the
//! next laps. Positions rather than inputs, so the replay can't drift and ignores terrain digging.

pub const STEP: u64 = 4;
const MAGIC: &[u8; 4] = b"RFG1";

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Atom positions, world units (as `SimCar::atom_positions`).
    pub atoms: Vec<[f32; 3]>,
    /// Steering angle, radians.
    pub steer: f32,
    /// Accumulated wheel rotation of atoms 0–3, radians.
    pub roll: [f32; 4],
}

#[derive(Default, Debug, PartialEq)]
pub struct Ghost {
    /// One frame per `STEP` ticks from the lap start.
    pub frames: Vec<Frame>,
}

impl Ghost {
    /// The ghost `ticks` into its lap, interpolated between samples; None past its finish.
    pub fn at(&self, ticks: u64) -> Option<Frame> {
        let i = (ticks / STEP) as usize;
        let (a, b) = (
            self.frames.get(i)?,
            self.frames.get(i + 1).unwrap_or(&self.frames[i]),
        );
        let t = (ticks % STEP) as f32 / STEP as f32;
        let mix = |x: f32, y: f32| x + (y - x) * t;
        Some(Frame {
            atoms: a
                .atoms
                .iter()
                .zip(&b.atoms)
                .map(|(p, q)| [mix(p[0], q[0]), mix(p[1], q[1]), mix(p[2], q[2])])
                .collect(),
            steer: mix(a.steer, b.steer),
            roll: [0, 1, 2, 3].map(|k| mix(a.roll[k], b.roll[k])),
        })
    }

    /// `RFG1`, atom count (u32), frame count (u32), then per frame: atoms × xyz, steer, roll × 4
    /// (all f32, little-endian).
    pub fn to_bytes(&self) -> Vec<u8> {
        let atoms = self.frames.first().map_or(0, |f| f.atoms.len());
        let mut out = MAGIC.to_vec();
        out.extend((atoms as u32).to_le_bytes());
        out.extend((self.frames.len() as u32).to_le_bytes());
        for f in &self.frames {
            let floats = f.atoms.iter().flatten().chain([&f.steer]).chain(&f.roll);
            out.extend(floats.flat_map(|v| v.to_le_bytes()));
        }
        out
    }

    pub fn from_bytes(data: &[u8]) -> Result<Ghost, String> {
        let u32_at = |o: usize| {
            data.get(o..o + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        };
        if data.get(..4) != Some(MAGIC) {
            return Err("not a ghost file".into());
        }
        let (atoms, frames) = (u32_at(4).ok_or("truncated")?, u32_at(8).ok_or("truncated")?);
        let per = atoms * 3 + 5;
        let body = &data[12..];
        if body.len() != frames * per * 4 {
            return Err(format!(
                "ghost file is {} bytes, expected {}",
                data.len(),
                12 + frames * per * 4
            ));
        }
        let floats: Vec<f32> = body
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect();
        Ok(Ghost {
            frames: floats
                .chunks_exact(per)
                .map(|f| Frame {
                    atoms: f[..atoms * 3].as_chunks::<3>().0.to_vec(),
                    steer: f[atoms * 3],
                    roll: [f[per - 4], f[per - 3], f[per - 2], f[per - 1]],
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(x: f32) -> Frame {
        Frame {
            atoms: vec![[x, 2.0 * x, 3.0], [0.0, 0.0, x]],
            steer: x / 10.0,
            roll: [x, 0.0, 0.0, -x],
        }
    }

    #[test]
    fn interpolates_and_ends() {
        let g = Ghost {
            frames: vec![frame(0.0), frame(8.0)],
        };
        assert_eq!(g.at(0), Some(frame(0.0)));
        assert_eq!(g.at(STEP / 2), Some(frame(4.0)));
        assert_eq!(g.at(STEP), Some(frame(8.0)));
        assert_eq!(g.at(STEP + 1), Some(frame(8.0))); // holds the last sample until the next one
        assert_eq!(g.at(2 * STEP), None);
        assert_eq!(Ghost::default().at(0), None);
    }

    #[test]
    fn round_trips_bytes() {
        let g = Ghost {
            frames: vec![frame(1.5), frame(-2.0), frame(7.0)],
        };
        let bytes = g.to_bytes();
        assert_eq!(Ghost::from_bytes(&bytes).unwrap(), g);
        assert!(Ghost::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        assert!(Ghost::from_bytes(b"nope").is_err());
        assert_eq!(
            Ghost::from_bytes(&Ghost::default().to_bytes()).unwrap(),
            Ghost::default()
        );
    }
}
