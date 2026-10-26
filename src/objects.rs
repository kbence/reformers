//! Scenery objects (#47): trees and the like, placed by OBJECTS.PCX and drawn from OBJTEX.PCX.
//!
//! OBJECTS.PCX is a 256×256 indexed image on the MAP/COL grid, one object per cell:
//! index >> 4 = kind (1–15), index & 15 = variant (size); 0 = nothing. A separate file from
//! CHECK.PCX, which makes a map a time trial and holds one rule per cell.
//!
//! OBJTEX.PCX is the sprite sheet: 256×256, colour 0 transparent, in its own palette. Kind k
//! is slot k − 1 of 32 slots, 32×64 pixels each, 8 per row; a sprite stands on its slot's
//! bottom-centre pixel.

use crate::{pcx, sim::Trunk, terrain::SIZE};

/// Sprite slot size in OBJTEX pixels.
pub const SLOT: [usize; 2] = [32, 64];
/// World units per sprite pixel at size 1 (a 64 px tree is ~12 m; a car is ~210 units, 4 m).
pub const PIXEL: f32 = 10.0;
const CELL: i32 = 256;

/// Each kind's collider at size 1 (#49): [radius, height] in world units, indexed by kind
/// (`[0]` unused); radius 0 = none, cars drive through (a bush). They come from the sheet's
/// OBJTEX.TXT, else from the sprite itself (see `colliders`).
pub type Colliders = [[u16; 2]; 16];

/// World units per sprite pixel at size 1, for the colliders (integer, as the sim needs).
const PIXEL_UNITS: u16 = 10;

/// The colliders for a sprite sheet: OBJTEX.TXT's `kind radius height` lines (`#` starts a
/// comment) where it lists a kind, else measured off the sprite: half the width of its painted
/// bottom row (the trunk) and its painted height. A kind with neither has none.
pub fn colliders(txt: Option<&str>, sheet: Option<&pcx::Image>) -> Result<Colliders, String> {
    let mut out: Colliders = std::array::from_fn(|k| {
        let Some(sheet) = sheet else { return [0, 0] };
        let Some(([x, y, w, h], _)) = sprite(sheet, k as u8) else {
            return [0, 0];
        };
        let bottom = (x..x + w).filter(|&px| sheet.pixels[(y + h - 1) * sheet.width + px] != 0);
        let (lo, hi) = (bottom.clone().min().unwrap_or(0), bottom.max().unwrap_or(0));
        let width = (hi - lo + 1) as u16;
        [(width * PIXEL_UNITS).div_ceil(2), h as u16 * PIXEL_UNITS]
    });
    for (n, line) in txt.unwrap_or("").lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let bad = || {
            format!(
                "OBJTEX.TXT line {}: want `kind radius height`, got {line:?}",
                n + 1
            )
        };
        let v: Vec<u16> = line
            .split_whitespace()
            .map(|f| f.parse().map_err(|_| bad()))
            .collect::<Result<_, _>>()?;
        match v[..] {
            [k @ 1..=15, r, h] => out[k as usize] = [r, h],
            _ => return Err(bad()),
        }
    }
    Ok(out)
}

/// OBJTEX.TXT for `c`: every kind, so what's saved is what's used.
pub fn colliders_text(c: &Colliders) -> String {
    let mut s = String::from(
        "# Object colliders (Reformers #49): kind, trunk radius, height, in world units at size 1\n\
         # (10 per sprite pixel; a car is ~210 long). Radius 0: cars drive through it.\n",
    );
    for (k, [r, h]) in c.iter().enumerate().skip(1) {
        s += &format!("{k} {r} {h}\n");
    }
    s
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Object {
    /// 1–15: the OBJTEX slot (kind − 1).
    pub kind: u8,
    /// Where it stands, world x, y. Integers, so the sim's colliders are the same on every machine.
    pub pos: [u16; 2],
    /// × the sprite's size in `PIXEL`s, in 256ths: the variant (0 = half, 5 = 1, 15 = 2) with
    /// ±10% jitter.
    pub size: u16,
}

impl Object {
    /// × the sprite's size in `PIXEL`s.
    pub fn scale(&self) -> f32 {
        self.size as f32 / 256.0
    }

    /// Its trunk in the sim, scaled with the sprite; None for kinds you drive through.
    pub fn trunk(&self, colliders: &Colliders) -> Option<Trunk> {
        let [r, h] =
            colliders[self.kind as usize].map(|v| ((v as u32 * self.size as u32) >> 8) as u16);
        (r > 0).then_some(Trunk {
            x: self.pos[0],
            y: self.pos[1],
            r,
            h,
        })
    }
}

/// OBJECTS.PCX's palette, so the file reads in a paint program: a colour per kind (pine dark
/// green, leafy tree green, bush light green, birch white, dead tree brown, rock grey, then
/// spare hues), brighter for bigger sizes; 0–15 (nothing) black.
pub fn palette() -> [[u8; 3]; 256] {
    const KINDS: [[u8; 3]; 15] = [
        [30, 100, 45],
        [70, 160, 50],
        [150, 200, 80],
        [235, 235, 220],
        [130, 90, 55],
        [150, 150, 155],
        [200, 60, 60],
        [60, 100, 220],
        [220, 180, 40],
        [180, 70, 200],
        [40, 190, 190],
        [230, 120, 30],
        [120, 200, 150],
        [200, 120, 160],
        [110, 110, 60],
    ];
    std::array::from_fn(|i| match i >> 4 {
        0 => [0; 3],
        k => {
            let shade = 0.55 + 0.45 * (i & 15) as f32 / 15.0;
            KINDS[k - 1].map(|c| (c as f32 * shade) as u8)
        }
    })
}

/// A fixed hash of a cell (any machine, any build): the jitter's source.
fn hash(x: usize, y: usize) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h
}

/// The objects an OBJECTS.PCX places, row by row. Each stands near its cell's centre: offset
/// up to 0.3 cells, scale ±10%, both from the cell's hash. Integer maths: the
/// sim collides with these positions.
pub fn load(objects_pcx: &[u8]) -> Result<Vec<Object>, String> {
    let img = pcx::decode_image(objects_pcx)?;
    if img.width < SIZE || img.height < SIZE {
        return Err(format!(
            "OBJECTS.PCX is {}×{}, needs at least {SIZE}×{SIZE}",
            img.width, img.height
        ));
    }
    let img = img.crop(SIZE, SIZE);
    Ok(img
        .pixels
        .iter()
        .enumerate()
        .filter(|&(_, &v)| v >> 4 != 0)
        .map(|(i, &v)| {
            let (x, y) = (i % SIZE, i / SIZE);
            let h = hash(x, y);
            let byte = |shift: u32| ((h >> shift) & 0xff) as i32;
            // ±0.3 cells: (0–255 − 128) × 0.6 = −76..76 units
            let at =
                |c: usize, shift| (c as i32 * CELL + CELL / 2 + (byte(shift) - 128) * 3 / 5) as u16;
            Object {
                kind: v >> 4,
                pos: [at(x, 0), at(y, 8)],
                // (5 + variant) / 10 × (230–281) / 256: 0.5–2, ±10%
                size: ((5 + (v & 15) as i32) * (230 + byte(16) / 5) / 10) as u16,
            }
        })
        .collect())
}

/// The sprite of `kind` in the sheet: its left, top, width and height in pixels, trimmed to
/// what's painted (colour ≠ 0), and where it stands (the slot's bottom centre) relative to that.
/// None for an empty slot.
pub fn sprite(sheet: &pcx::Image, kind: u8) -> Option<([usize; 4], [f32; 2])> {
    let slot = kind.checked_sub(1)? as usize;
    let (sx, sy) = (slot % 8 * SLOT[0], slot / 8 * SLOT[1]);
    if sy + SLOT[1] > sheet.height {
        return None;
    }
    let painted = |x: usize, y: usize| sheet.pixels[(sy + y) * sheet.width + sx + x] != 0;
    let xs: Vec<usize> = (0..SLOT[0])
        .filter(|&x| (0..SLOT[1]).any(|y| painted(x, y)))
        .collect();
    let ys: Vec<usize> = (0..SLOT[1])
        .filter(|&y| (0..SLOT[0]).any(|x| painted(x, y)))
        .collect();
    let (x0, x1, y0, y1) = (*xs.first()?, *xs.last()?, *ys.first()?, *ys.last()?);
    Some((
        [sx + x0, sy + y0, x1 - x0 + 1, y1 - y0 + 1],
        // the foot: the slot's bottom centre, in pixels from the trimmed box's top-left
        [SLOT[0] as f32 / 2.0 - x0 as f32, SLOT[1] as f32 - y0 as f32],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcx(pixels: Vec<u8>) -> Vec<u8> {
        pcx::encode(&pcx::Image {
            width: SIZE,
            height: SIZE,
            pixels,
            palette: [[0; 3]; 256],
        })
    }

    #[test]
    fn places_objects_near_their_cells() {
        let mut px = vec![0u8; SIZE * SIZE];
        px[10 * SIZE + 20] = 0x15; // kind 1, size 1
        px[11 * SIZE + 20] = 0x2f; // kind 2, size 2
        px[12 * SIZE + 20] = 0x07; // kind 0: nothing
        let objs = load(&pcx(px)).unwrap();
        assert_eq!(objs.len(), 2);
        let (a, b) = (objs[0], objs[1]);
        assert_eq!((a.kind, b.kind), (1, 2));
        for (o, (x, y)) in [(a, (20.0, 10.0)), (b, (20.0, 11.0))] {
            let cell = CELL as f32;
            let (dx, dy) = (
                o.pos[0] as f32 / cell - x - 0.5,
                o.pos[1] as f32 / cell - y - 0.5,
            );
            assert!(dx.abs() <= 0.3 && dy.abs() <= 0.3, "{o:?}");
        }
        assert!((0.89..=1.1).contains(&a.scale()), "{}", a.scale());
        assert!((1.79..=2.2).contains(&b.scale()), "{}", b.scale());
        let mut c: Colliders = [[0, 0]; 16];
        c[1] = [15, 620];
        let t = a.trunk(&c).unwrap();
        assert_eq!(
            ([t.x, t.y], t.r),
            (a.pos, ((15 * a.size as u32) >> 8) as u16)
        );
        let bush = Object { kind: 3, ..a };
        assert_eq!(bush.trunk(&c), None, "no collider: drive through");
        assert_ne!(a.pos, b.pos, "the jitter differs per cell");
        assert_eq!(load(&pcx(vec![0; SIZE * SIZE])).unwrap(), []);
    }

    /// terep2-forest's OBJECTS.PCX carries the palette, so it reads in a paint program.
    #[test]
    fn shipped_objects_have_the_palette() {
        crate::need_assets!();
        let b = crate::assets::read("maps/terep2-forest/OBJECTS.PCX").unwrap();
        assert_eq!(pcx::decode(&b).unwrap().palette, palette());
        assert!(!load(&b).unwrap().is_empty());
    }

    #[test]
    fn colliders_from_text_else_sprite() {
        let mut sheet = pcx::Image {
            width: SIZE,
            height: SIZE,
            pixels: vec![0; SIZE * SIZE],
            palette: [[0; 3]; 256],
        };
        // kind 1: a 20 px canopy over a 3 px trunk, 40 px tall in all
        for y in 24..64 {
            let (x0, x1) = if y < 50 { (6, 26) } else { (15, 18) };
            for x in x0..x1 {
                sheet.pixels[y * SIZE + x] = 7;
            }
        }
        let c = colliders(None, Some(&sheet)).unwrap();
        assert_eq!(
            c[1],
            [15, 400],
            "the trunk's half-width, the painted height"
        );
        assert_eq!(c[2], [0, 0], "no sprite, no collider");
        let txt = "# comment\n2 40 300\n\n1 0 0  # soft after all\n";
        let c = colliders(Some(txt), Some(&sheet)).unwrap();
        assert_eq!((c[1], c[2]), ([0, 0], [40, 300]));
        assert_eq!(
            colliders(Some(&colliders_text(&c)), None).unwrap(),
            c,
            "round trip"
        );
        assert!(colliders(Some("16 1 1"), None).is_err());
        assert!(colliders(Some("1 x 2"), None).is_err());
    }

    /// The shared sheet's OBJTEX.TXT gives what the starter sprites were measured at.
    #[test]
    fn shared_colliders() {
        crate::need_assets!();
        let txt = crate::assets::read("OBJTEX.TXT").unwrap();
        let sheet = pcx::decode(&crate::assets::read("OBJTEX.PCX").unwrap()).unwrap();
        let c = colliders(Some(&String::from_utf8(txt).unwrap()), Some(&sheet)).unwrap();
        assert_eq!(
            &c[1..7],
            [
                [15, 620],
                [20, 610],
                [0, 0],
                [15, 620],
                [15, 550],
                [120, 160]
            ]
        );
    }

    #[test]
    fn trims_sprites() {
        let mut sheet = pcx::Image {
            width: SIZE,
            height: SIZE,
            pixels: vec![0; SIZE * SIZE],
            palette: [[0; 3]; 256],
        };
        // kind 10 = slot 9: second row, second column → slot origin (32, 64)
        for y in 70..128 {
            for x in 40..50 {
                sheet.pixels[y * SIZE + x] = 5;
            }
        }
        let (rect, foot) = sprite(&sheet, 10).unwrap();
        assert_eq!(rect, [40, 70, 10, 58]);
        assert_eq!(foot, [16.0 - 8.0, 64.0 - 6.0]);
        assert_eq!(sprite(&sheet, 1), None, "an empty slot");
        assert_eq!(sprite(&sheet, 0), None, "kind 0 is nothing");
    }
}
