//! CAR*.DAT: atoms (point masses), springs and a renderer display list.
//! Layout per docs/re-notes.md; positions stay in the file's world units (16.16 → f32).

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AtomKind {
    Body,
    FrontWheel, // steered (atoms 0,1)
    RearWheel,
    Driver, // camera / driver atom
}

#[derive(Debug, Clone, Copy)]
pub struct Atom {
    pub pos: [f32; 3], // x = width, y = length (front is −y), z = up
    pub radius: f32,
    pub kind: AtomKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpringKind {
    /// Rigid, yields and breaks (type 1).
    Rigid,
    /// No force between min and max (type 0).
    Slack,
    /// Damped spring, softness = type/2 (types ≥ 2; suspension).
    Damped(u8),
}

#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub a: usize,
    pub b: usize,
    /// Lengths in world units (file stores ×64). `None` = take the current distance.
    pub rest: Option<f32>,
    /// Word +6: original rest (rigid) or previous length (slack/damped).
    pub orig: f32,
    pub min: f32,
    pub max: f32,
    pub kind: SpringKind,
    /// `mass shift` s: atom a gets 1/2^(s+1) of each correction.
    pub shift: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drive {
    Rear,
    Front,
    All,
}

/// A vertex reference into the original's projected-vertex buffer: 2i = atom i, 2i+1 = its
/// shadow on the ground; entries past the atoms are the steered front axle ends (`0x11f0`).
pub type Entry = u16;

/// Drawable primitives from the display list. Depth-ordering ops (2, 3, 16–20) are dropped:
/// a z-buffer replaces them.
#[derive(Debug, Clone)]
pub enum Prim {
    /// Op 4: flat polygon, palette colour.
    Flat { verts: Vec<Entry>, colour: u8 },
    /// Op 8: polygon textured from CARn.PCX; uv in skin pixels.
    Textured { verts: Vec<(Entry, [f32; 2])> },
    /// Op 10: wheel sprite at `centre`, axle towards `axle`; 9 view-angle frames.
    Wheel {
        centre: Entry,
        axle: Entry,
        frames: Vec<WheelFrame>,
    },
}

/// One view of a wheel sprite. Frames 0–8 sweep 180° round the wheel: 0 = the hub face-on
/// (looking down the axle), 4 = the tread edge-on, 8 = the inner side face-on.
#[derive(Debug, Clone, Copy)]
pub struct WheelFrame {
    /// Half width and half height on screen, world units (the height is the wheel's radius).
    pub half: [f32; 2],
    /// Corners in skin pixels: top-left, top-right, bottom-right, bottom-left.
    pub uv: [[f32; 2]; 4],
}

pub struct Car {
    pub drive: Drive,
    pub atoms: Vec<Atom>,
    pub springs: Vec<Spring>,
    pub prims: Vec<Prim>,
}

/// Walks the display list linearly (sub-lists are inline, so this visits every primitive).
fn parse_prims(d: &[u8]) -> Result<Vec<Prim>, String> {
    let w = |o: usize| {
        d.get(o..o + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or("display list truncated")
    };
    // uv words: high byte = pixel, low byte = fraction
    let uv = |o: usize| -> Result<[f32; 2], String> {
        Ok([w(o)? as f32 / 256.0, w(o + 2)? as f32 / 256.0])
    };
    let mut prims = Vec::new();
    let mut p = 0;
    while p < d.len() {
        let op = d[p];
        p += match op {
            0 | 6 | 11..=15 => 1,
            1 => 5,
            2 | 3 => 7,
            4 => {
                // n + 1 vertex words (the last repeats the first), then a colour word
                let n = *d.get(p + 1).ok_or("truncated")? as usize;
                let verts = (0..n).map(|i| w(p + 2 + 2 * i)).collect::<Result<_, _>>()?;
                prims.push(Prim::Flat {
                    verts,
                    colour: w(p + 2 + 2 * (n + 1))? as u8,
                });
                2 + 2 * (n + 1) + 2
            }
            5 => 2 + 4 * (*d.get(p + 1).ok_or("truncated")? as usize + 1),
            7 => 4 + 4 * (*d.get(p + 1).ok_or("truncated")? as usize + 1),
            8 => {
                let n = *d.get(p + 1).ok_or("truncated")? as usize;
                let verts = (0..n)
                    .map(|i| Ok((w(p + 2 + 6 * i)?, uv(p + 4 + 6 * i)?)))
                    .collect::<Result<_, String>>()?;
                prims.push(Prim::Textured { verts });
                2 + 6 * (n + 1)
            }
            9 => 21,
            10 => {
                let frames = (0..9)
                    .map(|f| {
                        let o = p + 7 + 20 * f;
                        Ok(WheelFrame {
                            half: [w(o + 2)? as f32 / 256.0, w(o)? as f32 / 256.0],
                            uv: [uv(o + 4)?, uv(o + 8)?, uv(o + 12)?, uv(o + 16)?],
                        })
                    })
                    .collect::<Result<_, String>>()?;
                prims.push(Prim::Wheel {
                    centre: w(p + 1)?,
                    axle: w(p + 3)?,
                    frames,
                });
                7 + 20 * 9
            }
            16 => 5,
            17..=20 => 3,
            _ => return Err(format!("unknown display-list op {op} at {p}")),
        };
    }
    Ok(prims)
}

pub fn parse(d: &[u8]) -> Result<Car, String> {
    let u16_at = |o: usize| {
        d.get(o..o + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or("truncated")
    };
    let i32_at = |o: usize| {
        d.get(o..o + 4)
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
            .ok_or("truncated")
    };
    let (atom_off, spring_off, list_off) = (
        u16_at(0)? as usize,
        u16_at(2)? as usize,
        u16_at(4)? as usize,
    );
    let drive = match u16_at(8)? {
        0 => Drive::Rear,
        1 => Drive::Front,
        _ => Drive::All,
    };

    let n = u16_at(atom_off)? as usize;
    if atom_off + 2 + n * 28 > spring_off {
        return Err(format!("atom section ({n} atoms) overlaps springs"));
    }
    let atoms = (0..n)
        .map(|i| {
            let o = atom_off + 2 + i * 28;
            let pos = [i32_at(o)?, i32_at(o + 4)?, i32_at(o + 8)?].map(|v| v as f32 / 65536.0);
            let kind = match u16_at(o + 0x1a)? {
                0 => AtomKind::Body,
                1 => AtomKind::RearWheel,
                2 => AtomKind::FrontWheel,
                0xffff => AtomKind::Driver,
                k => return Err(format!("atom {i}: unknown type {k}")),
            };
            Ok(Atom {
                pos,
                radius: u16_at(o + 0x18)? as f32,
                kind,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let m = u16_at(spring_off)? as usize;
    if spring_off + 2 + m * 14 > list_off {
        return Err(format!(
            "spring section ({m} springs) overlaps display list"
        ));
    }
    let springs = (0..m)
        .map(|i| {
            let o = spring_off + 2 + i * 14;
            let (a, b) = (u16_at(o)? as usize, u16_at(o + 2)? as usize);
            if a >= n || b >= n {
                return Err(format!("spring {i}: atom index out of range"));
            }
            let rest = u16_at(o + 4)? as i16;
            let (kind, shift) = (d[o + 8], d[o + 9]);
            Ok(Spring {
                a,
                b,
                rest: (rest >= 0).then(|| rest as f32 / 64.0),
                orig: u16_at(o + 6)? as f32 / 64.0,
                min: u16_at(o + 0xa)? as f32 / 64.0,
                max: u16_at(o + 0xc)? as f32 / 64.0,
                kind: match kind & 0x7f {
                    0 => SpringKind::Slack,
                    1 => SpringKind::Rigid,
                    k => SpringKind::Damped(k),
                },
                shift,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(Car {
        drive,
        atoms,
        springs,
        prims: parse_prims(&d[list_off..])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_game_cars() {
        crate::need_assets!();
        for n in 1..=5 {
            let car = parse(&crate::assets::read(&format!("CAR{n}.DAT")).unwrap()).unwrap();
            assert!(matches!(car.atoms.len(), 29 | 31), "CAR{n}");
            assert_eq!(car.atoms[0].kind, AtomKind::FrontWheel);
            assert_eq!(car.atoms[2].kind, AtomKind::RearWheel);
            assert_eq!(
                car.atoms
                    .iter()
                    .filter(|a| a.kind == AtomKind::Driver)
                    .count(),
                1
            );
            assert_eq!(car.drive, if n == 3 { Drive::Rear } else { Drive::All });
            assert_eq!(
                car.prims
                    .iter()
                    .filter(|p| matches!(p, Prim::Wheel { .. }))
                    .count(),
                4,
                "CAR{n}"
            );
            // wheel sprites: radius-high, narrowing towards edge-on (frame 4) and widening again
            for p in &car.prims {
                if let Prim::Wheel { centre, frames, .. } = p {
                    let r = car.atoms[*centre as usize / 2].radius;
                    let widths: Vec<f32> = frames.iter().map(|f| f.half[0]).collect();
                    assert!(frames.iter().all(|f| (f.half[1] - r).abs() < 1.0), "CAR{n}");
                    assert!(
                        widths[4] < widths[2] && widths[2] < widths[0],
                        "CAR{n}: {widths:?}"
                    );
                    assert!(
                        widths[4] < widths[6] && widths[6] < widths[8],
                        "CAR{n}: {widths:?}"
                    );
                }
            }
            assert_eq!(
                car.prims
                    .iter()
                    .filter(|p| matches!(p, Prim::Textured { .. }))
                    .count(),
                15,
                "CAR{n}"
            );
            let max_entry = 2 * car.atoms.len() as u16 + 2;
            for p in &car.prims {
                if let Prim::Flat { verts, .. } = p {
                    assert!(
                        verts.iter().all(|&e| e < max_entry),
                        "CAR{n} entry out of range"
                    );
                }
            }
            // Rigid rest lengths match the atom distances in the file.
            for s in car.springs.iter().filter(|s| s.kind == SpringKind::Rigid) {
                let (p, q) = (car.atoms[s.a].pos, car.atoms[s.b].pos);
                let dist = (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f32>().sqrt();
                assert!(
                    (s.rest.unwrap() - dist).abs() < 0.1,
                    "CAR{n} spring {}-{}",
                    s.a,
                    s.b
                );
            }
        }
    }
}
