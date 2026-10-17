//! 256×256 terrain from MAP.PCX (heights) + COL.PCX (colour = surface material); TEREP1's BMPs too.
//! Coordinates follow the original: 1 cell = 256 world units, height = MAP index × 16.

use crate::pcx;

pub const SIZE: usize = 256;

#[derive(Clone)]
pub struct Terrain {
    /// Raw MAP indices, mutable: the game digs by decrementing them.
    pub height: Vec<u8>,
    /// Raw COL indices: colour for rendering, `>> 4` = material for physics.
    pub colour: Vec<u8>,
    pub palette: [[u8; 3]; 256],
}

impl Terrain {
    pub fn load(map_pcx: &[u8], col_pcx: &[u8]) -> Result<Terrain, String> {
        let map = pcx::decode_image(map_pcx)?.crop(SIZE, SIZE);
        let col = pcx::decode_image(col_pcx)?.crop(SIZE, SIZE);
        Ok(Terrain {
            height: map.pixels,
            colour: col.pixels,
            palette: col.palette,
        })
    }

    fn h(&self, cx: usize, cy: usize) -> f32 {
        // ponytail: wraps at the map edge; the game keeps cars 0x80 units inside it.
        self.height[(cy % SIZE) * SIZE + cx % SIZE] as f32 * 16.0
    }

    /// Height at world (x, y), interpolated on the cell's two triangles like `0x25c5`.
    pub fn height_at(&self, x: f32, y: f32) -> f32 {
        let (cx, cy) = ((x / 256.0).floor(), (y / 256.0).floor());
        let (fx, fy) = (x / 256.0 - cx, y / 256.0 - cy);
        let (cx, cy) = (
            cx.rem_euclid(SIZE as f32) as usize,
            cy.rem_euclid(SIZE as f32) as usize,
        );
        if fx + fy <= 1.0 {
            let h0 = self.h(cx, cy);
            h0 + fx * (self.h(cx + 1, cy) - h0) + fy * (self.h(cx, cy + 1) - h0)
        } else {
            let h1 = self.h(cx + 1, cy + 1);
            h1 + (1.0 - fx) * (self.h(cx, cy + 1) - h1) + (1.0 - fy) * (self.h(cx + 1, cy) - h1)
        }
    }

    /// Surface material 0–15 (index into the friction tables in docs/re-notes.md).
    pub fn material_at(&self, x: f32, y: f32) -> u8 {
        let (cx, cy) = ((x / 256.0) as usize % SIZE, (y / 256.0) as usize % SIZE);
        self.colour[cy * SIZE + cx] >> 4
    }

    /// Heights normalised to 0.0–1.0 (index / 255), row-major, for vertex buffers / textures.
    pub fn heights_f32(&self) -> Vec<f32> {
        self.height.iter().map(|&h| h as f32 / 255.0).collect()
    }

    pub fn colour_rgb(&self) -> Vec<u8> {
        self.colour
            .iter()
            .flat_map(|&i| self.palette[i as usize])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_with_bump() -> Terrain {
        let mut height = vec![0u8; SIZE * SIZE];
        height[SIZE + 1] = 4; // cell (1,1) corner = 64 units
        Terrain {
            height,
            colour: vec![0x37; SIZE * SIZE],
            palette: [[0; 3]; 256],
        }
    }

    #[test]
    fn height_interpolates_on_triangles() {
        let t = flat_with_bump();
        assert_eq!(t.height_at(256.0, 256.0), 64.0);
        assert_eq!(t.height_at(128.0, 128.0), 0.0); // on the diagonal, lower triangle: corners all 0
        assert_eq!(t.height_at(192.0, 192.0), 32.0); // upper triangle, halfway to (1,1)
        assert_eq!(t.height_at(384.0, 256.0), 32.0); // halfway along the edge to (2,1)
        assert_eq!(t.material_at(10.0, 10.0), 3);
    }

    #[test]
    fn loads_game_map() {
        crate::need_assets!();
        let read = |f: &str| crate::assets::read(f).unwrap();
        let t = Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap();
        let t1 = Terrain::load(&read("maps/terep1/MAP.BMP"), &read("maps/terep1/COL.BMP")).unwrap();
        assert_ne!(t1.height, t.height);
        assert_eq!(t.heights_f32().len(), SIZE * SIZE);
        assert_eq!(t.colour_rgb().len(), SIZE * SIZE * 3);
    }
}
