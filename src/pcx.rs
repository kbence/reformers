//! 8-bit RLE PCX (ZSoft v5): single-plane with a 256-colour palette, the flavour TEREP2 ships,
//! or 24-bit (3 planes), of which only the first (red) plane is kept: one community height map
//! (the pack's TUNDRA) was saved that way, and height maps are grey, so red is the height.

#[derive(Clone, Debug)]
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

/// A map image: TEREP2's PCX, TEREP1's BMP, or a greyscale height map (PNG / binary PGM, #35).
pub fn decode_image(data: &[u8]) -> Result<Image, String> {
    if data.starts_with(b"BM") {
        crate::bmp::decode(data)
    } else if data.starts_with(b"\x89PNG") || data.starts_with(b"P5") {
        decode_grey(data)
    } else {
        decode(data)
    }
}

/// A greyscale height map: grey level = height index (×16 world units), as heightmap tools
/// paint them. PNG (any depth, through macroquad's decoder; colour is averaged to grey) or
/// binary PGM. The palette is a grey ramp, so the image shows as painted.
pub fn decode_grey(data: &[u8]) -> Result<Image, String> {
    let (width, height, pixels) = if data.starts_with(b"P5") {
        // header: P5, width, height, maxval, each after whitespace (and # comments), then one
        // whitespace byte and the samples
        let mut fields = vec![];
        let mut i = 2;
        while fields.len() < 3 {
            match data.get(i).ok_or("PGM header ends early")? {
                b'#' => {
                    while data.get(i).is_some_and(|&c| c != b'\n') {
                        i += 1;
                    }
                }
                c if c.is_ascii_digit() => {
                    let start = i;
                    while data.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    let text = std::str::from_utf8(&data[start..i]).unwrap();
                    fields.push(text.parse::<usize>().map_err(|e| e.to_string())?);
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        let [w, h, max] = [fields[0], fields[1], fields[2]];
        if max == 0 || max > 255 {
            return Err(format!("PGM maxval {max}: only 8-bit PGMs are supported"));
        }
        let px = data
            .get(i + 1..i + 1 + w * h)
            .ok_or("PGM pixel data ends early")?;
        (
            w,
            h,
            px.iter().map(|&v| (v as usize * 255 / max) as u8).collect(),
        )
    } else {
        let img = macroquad::texture::Image::from_file_with_format(
            data,
            Some(macroquad::prelude::ImageFormat::Png),
        )
        .map_err(|e| format!("PNG: {e}"))?;
        let px = img
            .bytes
            .chunks_exact(4)
            .map(|p| ((p[0] as u16 + p[1] as u16 + p[2] as u16 + 1) / 3) as u8)
            .collect();
        (img.width as usize, img.height as usize, px)
    };
    Ok(Image {
        width,
        height,
        pixels,
        palette: std::array::from_fn(|i| [i as u8; 3]),
    })
}

/// An 8-bit RLE PCX (ZSoft v5) with a 256-colour palette: what TEREP2 and the DOS editor read.
pub fn encode(img: &Image) -> Vec<u8> {
    let bytes_per_line = img.width + img.width % 2; // even, as the format wants
    let mut out = vec![0u8; 128];
    out[..4].copy_from_slice(&[0x0A, 5, 1, 8]);
    out[8..10].copy_from_slice(&(img.width as u16 - 1).to_le_bytes());
    out[10..12].copy_from_slice(&(img.height as u16 - 1).to_le_bytes());
    out[12..16].copy_from_slice(&[72, 0, 72, 0]); // dpi
    out[65] = 1;
    out[66..68].copy_from_slice(&(bytes_per_line as u16).to_le_bytes());
    out[68] = 1; // colour palette
    for row in img.pixels.chunks_exact(img.width) {
        let mut line = row.to_vec();
        line.resize(bytes_per_line, 0);
        let mut i = 0;
        while i < line.len() {
            let v = line[i];
            let run = line[i..].iter().take(63).take_while(|&&x| x == v).count();
            if run > 1 || v >= 0xC0 {
                out.extend([0xC0 | run as u8, v]);
            } else {
                out.push(v);
            }
            i += run;
        }
    }
    out.push(0x0C);
    out.extend(img.palette.iter().flatten());
    out
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
    fn encodes_what_it_decodes() {
        crate::need_assets!();
        for f in ["maps/terep2/MAP.PCX", "CAR1.PCX"] {
            let img = decode(&crate::assets::read(f).unwrap()).unwrap();
            let back = decode(&encode(&img)).unwrap();
            assert_eq!(
                (back.width, back.height, &back.pixels, back.palette),
                (img.width, img.height, &img.pixels, img.palette),
                "{f}"
            );
        }
        // odd width (padded line), runs longer than 63, literal bytes ≥ 0xC0
        let odd = Image {
            width: 3,
            height: 2,
            pixels: vec![0xC5, 1, 1, 7, 7, 7],
            palette: [[9; 3]; 256],
        };
        assert_eq!(decode(&encode(&odd)).unwrap().pixels, odd.pixels);
        let long = Image {
            width: 200,
            height: 1,
            pixels: vec![42; 200],
            palette: [[0; 3]; 256],
        };
        assert_eq!(decode(&encode(&long)).unwrap().pixels, long.pixels);
    }

    #[test]
    fn reads_greyscale_height_maps() {
        let pgm = b"P5\n# a comment\n3 2\n255\n\x00\x10\xff\x80\x40\x01".to_vec();
        let img = decode_image(&pgm).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.pixels, [0, 0x10, 0xff, 0x80, 0x40, 1]);
        assert_eq!(img.palette[0x80], [0x80; 3]);
        let half = b"P5 1 1 127 \x7f".to_vec();
        assert_eq!(
            decode_image(&half).unwrap().pixels,
            [255],
            "maxval scales to 0–255"
        );
        assert!(decode_image(b"P5 2 2 255\n\x00").is_err(), "short data");
        assert!(
            decode_image(b"P5 1 1 65535\n\x00\x00").is_err(),
            "16-bit PGM"
        );
        // PNG, as a paint program saves it (written here through macroquad's encoder, which
        // stores rows bottom-up as it's meant for screenshots: hand it the rows flipped)
        let path = std::env::temp_dir().join(format!("reformers-{}.png", std::process::id()));
        let grey = [0u8, 0x10, 0xff, 0x80, 0x40, 1];
        macroquad::texture::Image {
            width: 3,
            height: 2,
            bytes: grey
                .chunks(3)
                .rev()
                .flatten()
                .flat_map(|&v| [v, v, v, 255])
                .collect(),
        }
        .export_png(path.to_str().unwrap());
        let png = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(decode_image(&png).unwrap().pixels, grey);
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
