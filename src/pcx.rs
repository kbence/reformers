//! 8-bit RLE PCX (ZSoft v5): single-plane with a 256-colour palette, the flavour TEREP2 ships,
//! or 24-bit (3 planes), of which only the first (red) plane is kept: one community height map
//! (the pack's TUNDRA) was saved that way, and height maps are grey, so red is the height.

#[derive(Debug)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Palette indices, row-major, `width * height`.
    pub pixels: Vec<u8>,
    /// 256 VGA colours, RGB.
    pub palette: [[u8; 3]; 256],
}

pub fn decode(data: &[u8]) -> Result<Image, String> {
    if data.len() < 128 + 769 {
        return Err("file too short".into());
    }
    let u16_at = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]) as usize;
    let (manufacturer, encoding, bpp, planes) = (data[0], data[2], data[3], data[65]);
    if manufacturer != 0x0A || encoding != 1 || bpp != 8 || !matches!(planes, 1 | 3) {
        return Err(format!(
            "unsupported PCX: manufacturer={manufacturer:#x} encoding={encoding} bpp={bpp} planes={planes}"
        ));
    }
    let width = u16_at(8) - u16_at(4) + 1;
    let height = u16_at(10) - u16_at(6) + 1;
    let bytes_per_line = u16_at(66);
    if bytes_per_line < width {
        return Err(format!("bytes_per_line {bytes_per_line} < width {width}"));
    }

    let planes = planes as usize;
    // 24-bit images have no palette: a grey ramp, so their first plane reads as itself
    let mut palette = std::array::from_fn(|i| [i as u8; 3]);
    let pal_start = if planes == 1 {
        let start = data.len() - 769;
        if data[start] != 0x0C {
            return Err("missing 256-colour palette marker".into());
        }
        for (i, c) in palette.iter_mut().enumerate() {
            c.copy_from_slice(&data[start + 1 + i * 3..][..3]);
        }
        start
    } else {
        data.len()
    };
    let line = bytes_per_line * planes; // a scanline holds each plane in turn

    // Decode the whole stream first: some encoders let runs cross scanlines.
    let mut raw = Vec::with_capacity(line * height);
    let mut rle = data[128..pal_start].iter();
    while raw.len() < line * height {
        let &b = rle.next().ok_or("RLE data ends early")?;
        if b >= 0xC0 {
            let &v = rle.next().ok_or("RLE data ends mid-run")?;
            raw.extend(std::iter::repeat_n(v, (b & 0x3F) as usize));
        } else {
            raw.push(b);
        }
    }
    raw.truncate(line * height);

    let pixels = raw
        .chunks_exact(line)
        .flat_map(|row| &row[..width])
        .copied()
        .collect();
    Ok(Image {
        width,
        height,
        pixels,
        palette,
    })
}

/// A map image in either format the games used: TEREP2's PCX or TEREP1's BMP.
pub fn decode_image(data: &[u8]) -> Result<Image, String> {
    if data.starts_with(b"BM") {
        crate::bmp::decode(data)
    } else {
        decode(data)
    }
}

impl Image {
    /// Top-left `w`×`h` region. TEREP2's 320×256 maps keep the map in columns
    /// 0–255; columns 256–319 are a legend/label sidebar.
    pub fn crop(&self, w: usize, h: usize) -> Image {
        assert!(w <= self.width && h <= self.height);
        let pixels = self
            .pixels
            .chunks_exact(self.width)
            .take(h)
            .flat_map(|r| &r[..w])
            .copied()
            .collect();
        Image {
            width: w,
            height: h,
            pixels,
            palette: self.palette,
        }
    }

    pub fn to_rgb(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|&i| self.palette[i as usize])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcx(width: u16, height: u16, bytes_per_line: u16, rle: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 128];
        d[0] = 0x0A;
        d[1] = 5;
        d[2] = 1;
        d[3] = 8;
        d[8..10].copy_from_slice(&(width - 1).to_le_bytes());
        d[10..12].copy_from_slice(&(height - 1).to_le_bytes());
        d[65] = 1;
        d[66..68].copy_from_slice(&bytes_per_line.to_le_bytes());
        d.extend_from_slice(rle);
        d.push(0x0C);
        d.extend((0..=255u8).flat_map(|i| [i, 255 - i, 7]));
        d
    }

    #[test]
    fn decodes_runs_literals_padding_and_crop() {
        // 3×2, padded to 4 bytes/line. Row 0: run of 3×9 + pad; row 1: literal 0xC5
        // (must be escaped as a run of 1), literal 2, run crossing into pad.
        let img = decode(&pcx(3, 2, 4, &[0xC3, 9, 0, 0xC1, 0xC5, 2, 0xC2, 4])).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.pixels, [9, 9, 9, 0xC5, 2, 4]);
        assert_eq!(img.palette[9], [9, 246, 7]);
        let c = img.crop(2, 2);
        assert_eq!(c.pixels, [9, 9, 0xC5, 2]);
        assert_eq!(&c.to_rgb()[..3], &[9, 246, 7]);
        assert!(decode(&pcx(3, 2, 4, &[0xC3, 9])).is_err());
    }

    #[test]
    fn keeps_the_first_plane_of_24_bit_images() {
        // 2×2, 3 planes: per row R R G G B B; no palette (padded to the minimum file size)
        let mut d = pcx(2, 2, 2, &[10, 11, 20, 21, 30, 31, 12, 0xC3, 40]);
        d[65] = 3;
        d.truncate(128 + 9);
        d.resize(128 + 769, 0);
        let img = decode(&d).unwrap();
        assert_eq!(img.pixels, [10, 11, 12, 40]);
        assert_eq!(
            img.palette[40],
            [40, 40, 40],
            "a grey ramp stands in for the palette"
        );
    }

    #[test]
    fn decodes_game_maps() {
        crate::need_assets!();
        for f in [
            "maps/terep2/MAP.PCX",
            "maps/terep2/COL.PCX",
            "MAPTEX.PCX",
            "maps/terep2-hills/MAP.PCX",
        ] {
            let img = decode(&crate::assets::read(f).unwrap()).unwrap();
            assert_eq!((img.width, img.height), (320, 256), "{f}");
        }
        for f in ["CAR1.PCX", "TEXTURES.PCX"] {
            let img = decode(&crate::assets::read(f).unwrap()).unwrap();
            assert_eq!((img.width, img.height), (320, 200), "{f}");
        }
    }
}
