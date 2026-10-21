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
    /// Per primitive: one side of a two-sided panel (see `twins`), drawn only from its front.
    pub one_sided: Vec<bool>,
}

/// The atoms a flat or textured polygon is made of; None for shadows, axle ends and the rest.
pub fn face_atoms(prim: &Prim, atoms: usize) -> Option<Vec<usize>> {
    let entries: Vec<Entry> = match prim {
        Prim::Flat { verts, .. } => verts.clone(),
        Prim::Textured { verts } => verts.iter().map(|v| v.0).collect(),
        Prim::Wheel { .. } => return None,
    };
    if entries
        .iter()
        .any(|&e| e % 2 == 1 || e as usize >= 2 * atoms)
    {
        return None;
    }
    let mut a: Vec<usize> = entries.iter().map(|&e| e as usize / 2).collect();
    a.dedup();
    if a.len() > 1 && a[0] == a[a.len() - 1] {
        a.pop(); // the closing vertex
    }
    (a.len() >= 3).then_some(a)
}

/// A polygon's front normal (Newell's method, not normalised): towards the side it's meant to
/// be seen from, e.g. up for the shipped cars' painted roofs. The car's coordinates are
/// left-handed (y runs front to back), hence the sign.
pub fn normal(p: &[[f32; 3]]) -> [f32; 3] {
    let mut n = [0.0f32; 3];
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

/// The two sides of a panel: polygons on the same plane, sharing three atoms or more, wound
/// opposite ways (a roof's dark underside and painted top). The original drew whichever faced
/// the camera (display-list back-face tests); a depth buffer would draw both, flickering.
fn twins(atoms: &[Atom], prims: &[Prim]) -> Vec<bool> {
    let faces: Vec<Option<(Vec<usize>, [f32; 3])>> = prims
        .iter()
        .map(|p| {
            let a = face_atoms(p, atoms.len())?;
            let n = normal(&a.iter().map(|&i| atoms[i].pos).collect::<Vec<_>>());
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            (len > 1e-3).then(|| (a, n.map(|v| v / len)))
        })
        .collect();
    let mut out = vec![false; prims.len()];
    for i in 0..faces.len() {
        for j in i + 1..faces.len() {
            let (Some((a, na)), Some((b, nb))) = (&faces[i], &faces[j]) else {
                continue;
            };
            let shared = a.iter().filter(|x| b.contains(x)).count();
            let opposite = na[0] * nb[0] + na[1] * nb[1] + na[2] * nb[2] < -0.99;
            if shared >= 3 && opposite {
                (out[i], out[j]) = (true, true);
            }
        }
    }
    out
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

    let prims = parse_prims(&d[list_off..])?;
    Ok(Car {
        drive,
        one_sided: twins(&atoms, &prims),
        atoms,
        springs,
        prims,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panels seen from both sides: roof and side pillars in every car, CAR3's lower body
    /// too. The painted roof's winding faces up, its dark underside's down.
    #[test]
    fn finds_two_sided_panels() {
        crate::need_assets!();
        for (n, faces) in [(1, 6), (2, 6), (3, 14), (4, 6), (5, 6)] {
            let car = parse(&crate::assets::read(&format!("CAR{n}.DAT")).unwrap()).unwrap();
            assert_eq!(
                car.one_sided.iter().filter(|&&o| o).count(),
                faces,
                "CAR{n}"
            );
            let roof: Vec<(u8, f32)> = (car.prims.iter().zip(&car.one_sided))
                .filter(|&(_, &o)| o)
                .filter_map(|(p, _)| match p {
                    Prim::Flat { colour, .. } => {
                        let a = face_atoms(p, car.atoms.len())?;
                        let mut sorted = a.clone();
                        sorted.sort();
                        (sorted == [12, 13, 14, 15]).then(|| {
                            let pts: Vec<_> = a.iter().map(|&i| car.atoms[i].pos).collect();
                            (*colour, normal(&pts)[2])
                        })
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(roof.len(), 2, "CAR{n}");
            for (colour, up) in roof {
                assert_eq!(up > 0.0, colour != 4, "CAR{n}: colour {colour} faces {up}");
            }
        }
    }

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
