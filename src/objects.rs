//! Scenery objects (#47): trees and the like, placed by OBJECTS.PCX and drawn from OBJTEX.PCX.
//!
//! OBJECTS.PCX is a 256×256 indexed image on the MAP/COL grid, one object per cell:
//! index >> 4 = kind (1–15), index & 15 = variant (size); 0 = nothing. A separate file from
//! CHECK.PCX, which makes a map a time trial and holds one rule per cell.
//!
//! OBJTEX.PCX is the sprite sheet: 256×256, colour 0 transparent, in its own palette. Kind k
//! is slot k − 1 of 32 slots, 32×64 pixels each, 8 per row; a sprite stands on its slot's
//! bottom-centre pixel.

use crate::{pcx, terrain::SIZE};

/// Sprite slot size in OBJTEX pixels.
pub const SLOT: [usize; 2] = [32, 64];
/// World units per sprite pixel at size 1 (a 64 px tree is ~12 m; a car is ~210 units, 4 m).
pub const PIXEL: f32 = 10.0;
const CELL: f32 = 256.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Object {
    /// 1–15: the OBJTEX slot (kind − 1).
    pub kind: u8,
    /// Where it stands, world x, y.
    pub pos: [f32; 2],
    /// × the sprite's size in `PIXEL`s: the variant (0 = half, 5 = 1, 15 = 2) with ±10% jitter.
    pub scale: f32,
    /// Mirrored, for variety.
    pub flip: bool,
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
/// up to 0.3 cells, scale ±10%, maybe mirrored, all from the cell's hash.
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
            let unit = |shift: u32| ((h >> shift) & 0xff) as f32 / 255.0; // 0–1
            Object {
                kind: v >> 4,
                pos: [
                    (x as f32 + 0.5 + (unit(0) - 0.5) * 0.6) * CELL,
                    (y as f32 + 0.5 + (unit(8) - 0.5) * 0.6) * CELL,
                ],
                scale: (0.5 + (v & 15) as f32 / 10.0) * (0.9 + unit(16) * 0.2),
                flip: h >> 31 == 1,
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
            let (dx, dy) = (o.pos[0] / CELL - x - 0.5, o.pos[1] / CELL - y - 0.5);
            assert!(dx.abs() <= 0.3 && dy.abs() <= 0.3, "{o:?}");
        }
        assert!((0.9..=1.1).contains(&a.scale), "{}", a.scale);
        assert!((1.8..=2.2).contains(&b.scale), "{}", b.scale);
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
