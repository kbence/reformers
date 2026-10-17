//! 8-bit uncompressed Windows BMP — TEREP1's map format.

use crate::pcx::Image;

pub fn decode(data: &[u8]) -> Result<Image, String> {
    let u32_at = |o: usize| {
        data.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .ok_or("BMP header too short")
    };
    if data.get(..2) != Some(b"BM") {
        return Err("not a BMP".into());
    }
    let (offset, header) = (u32_at(10)? as usize, u32_at(14)? as usize);
    let (width, height) = (u32_at(18)? as i32, u32_at(22)? as i32);
    let (bpp, compression) = (data[28], u32_at(30)?);
    if bpp != 8 || compression != 0 || width <= 0 || height == 0 {
        return Err(format!(
            "unsupported BMP: {width}×{height}, {bpp} bpp, compression {compression} (need 8-bit uncompressed)"
        ));
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    let colours = match u32_at(46)? {
        0 => 256,
        n => (n as usize).min(256),
    };
    let mut palette = [[0u8; 3]; 256];
    for (i, c) in palette.iter_mut().take(colours).enumerate() {
        let bgr = data
            .get(14 + header + i * 4..14 + header + i * 4 + 3)
            .ok_or("BMP palette cut short")?;
        *c = [bgr[2], bgr[1], bgr[0]];
    }
    let stride = (w + 3) & !3;
    let rows = data
        .get(offset..offset + stride * h)
        .ok_or("BMP pixel data cut short")?;
    // rows are stored bottom-up unless the height is negative
    let pixels = (0..h)
        .flat_map(|y| {
            let r = if height > 0 { h - 1 - y } else { y };
            &rows[r * stride..r * stride + w]
        })
        .copied()
        .collect();
    Ok(Image {
        width: w,
        height: h,
        pixels,
        palette,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3×2 bottom-up BMP (rows padded to 4 bytes) with a 2-colour palette.
    fn bmp(height: i32) -> Vec<u8> {
        let mut d = b"BM".to_vec();
        d.extend([0u8; 8]);
        d.extend((14u32 + 40 + 8).to_le_bytes()); // pixel offset
        d.extend(40u32.to_le_bytes());
        d.extend(3i32.to_le_bytes());
        d.extend(height.to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(8u16.to_le_bytes());
        d.extend([0u8; 16]); // compression .. y ppm
        d.extend(2u32.to_le_bytes()); // colours used
        d.extend(0u32.to_le_bytes());
        d.extend([30, 20, 10, 0, 3, 2, 1, 0]); // BGRA × 2
        d.extend([1, 0, 1, 0, 0, 1, 0, 0]); // stored row 0, row 1
        d
    }

    #[test]
    fn decodes_bottom_up_and_top_down() {
        let up = decode(&bmp(2)).unwrap();
        assert_eq!((up.width, up.height), (3, 2));
        assert_eq!(up.pixels, [0, 1, 0, 1, 0, 1]); // last stored row first
        assert_eq!((up.palette[0], up.palette[1]), ([10, 20, 30], [1, 2, 3]));
        assert_eq!(decode(&bmp(-2)).unwrap().pixels, [1, 0, 1, 0, 1, 0]);
        assert!(decode(b"PCX?").is_err());
    }

    #[test]
    fn decodes_terep1_maps() {
        crate::need_assets!();
        for f in ["maps/terep1/MAP.BMP", "maps/terep1/COL.BMP"] {
            let img = decode(&crate::assets::read(f).unwrap()).unwrap();
            assert_eq!((img.width, img.height), (256, 256), "{f}");
        }
    }
}
