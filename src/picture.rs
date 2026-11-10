// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: AGPL-3.0-or-later
// Images viewed from the Browse tab: PNG, JPEG and BMP (Luma3DS screenshots
// are 400 x 240 or 320 x 240 BMPs), read whole and fitted to the top screen.
// Decoding happens on the main loop, so it is kept cheap: JPEGs decode
// straight at a reduced size, big PNGs are refused.

use crate::sys::File;

/// the top screen
pub const W: usize = 400;
pub const H: usize = 240;

/// bigger files aren't read (the 3DS has little memory)
const MAX_FILE: u64 = 16 << 20;
/// the most pixels decoded at once: 3 Mpx is 12 MB of RGBA, then as much again
/// as 0xRRGGBB (PNGs and BMPs decode at full size, JPEGs at the reduced one)
const MAX_PIXELS: u64 = 3_000_000;

pub struct Picture {
    pub w: usize, // the image, fitted
    pub h: usize,
    pub px: Vec<u32>,  // 0xRRGGBB, w x h
    pub full: (u32, u32), // the image's own size
}

pub fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".bmp"].iter().any(|e| lower.ends_with(e))
}

/// an image decoded to 0xRRGGBB, at its size or a reduced one
struct Raw {
    w: usize,
    h: usize,
    px: Vec<u32>,
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    let f = File::open_read(path).ok_or("Can't open the file")?;
    let size = f.size().ok_or("Can't read the file")?;
    if size > MAX_FILE {
        return Err("Too big to show (over 16 MB)".into());
    }
    let mut data = vec![0u8; size as usize];
    let mut off = 0;
    while off < data.len() {
        match f.read_at(off as u64, &mut data[off..]) {
            Some(n) if n > 0 => off += n,
            _ => return Err("SD card read error".into()),
        }
    }
    Ok(data)
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// takes the file, to free it before the pixels are converted
fn png(data: Vec<u8>) -> Result<(Raw, (u32, u32)), String> {
    let mut d = png::Decoder::new(std::io::Cursor::new(data));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().map_err(|_| "Not a PNG Cement can read")?;
    let (w, h) = (r.info().width, r.info().height);
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("Too big to show ({w} x {h})"));
    }
    let mut buf = vec![0; r.output_buffer_size().ok_or("Too big to show")?];
    let out = r.next_frame(&mut buf).map_err(|_| "This PNG is damaged")?;
    drop(r);
    let ch = out.color_type.samples();
    let px = buf[..out.buffer_size()]
        .chunks_exact(ch)
        .map(|p| match ch {
            1 | 2 => rgb(p[0], p[0], p[0]),
            _ => rgb(p[0], p[1], p[2]),
        })
        .collect();
    Ok((Raw { w: w as usize, h: h as usize, px }, (w, h)))
}

/// takes the file, to free it before the pixels are converted
fn jpeg(data: Vec<u8>) -> Result<(Raw, (u32, u32)), String> {
    let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    d.read_info().map_err(|_| "Not a JPEG Cement can read")?;
    let info = d.info().ok_or("Not a JPEG Cement can read")?;
    let full = (info.width as u32, info.height as u32);
    // a progressive JPEG keeps every coefficient of the full image while it decodes
    let progressive = info.coding_process == jpeg_decoder::CodingProcess::DctProgressive;
    if progressive && full.0 as u64 * full.1 as u64 > MAX_PIXELS {
        return Err(format!("Too big to show ({} x {})", full.0, full.1));
    }
    // the smallest 1/8 step still covering the screen: much less to decode
    let (w, h) = d.scale(W as u16, H as u16).map_err(|_| "Not a JPEG Cement can read")?;
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("Too big to show ({} x {})", full.0, full.1));
    }
    let pixels = d.decode().map_err(|_| "This JPEG is damaged")?;
    let fmt = d.info().map(|i| i.pixel_format);
    drop(d);
    let px = match fmt {
        Some(jpeg_decoder::PixelFormat::L8) => pixels.iter().map(|&l| rgb(l, l, l)).collect(),
        Some(jpeg_decoder::PixelFormat::RGB24) => pixels.chunks_exact(3).map(|p| rgb(p[0], p[1], p[2])).collect(),
        Some(jpeg_decoder::PixelFormat::CMYK32) => pixels
            .chunks_exact(4)
            .map(|p| {
                let k = 255 - p[3] as u32;
                let c = |v: u8| ((255 - v as u32) * k / 255) as u8;
                rgb(c(p[0]), c(p[1]), c(p[2]))
            })
            .collect(),
        _ => return Err("This JPEG's colours aren't supported".into()),
    };
    Ok((Raw { w: w as usize, h: h as usize, px }, full))
}

/// uncompressed 24 or 32 bits per pixel, as Luma3DS and most tools write them;
/// takes the file like the others, to free it before the image is fitted
fn bmp(d: Vec<u8>) -> Result<(Raw, (u32, u32)), String> {
    let u16_at = |i: usize| d.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |i: usize| d.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let bad = || "Not a BMP Cement can read".to_string();
    if !d.starts_with(b"BM") {
        return Err(bad());
    }
    let start = u32_at(10).ok_or_else(bad)? as usize;
    let w = u32_at(18).ok_or_else(bad)? as i32;
    let h = u32_at(22).ok_or_else(bad)? as i32;
    let bpp = u16_at(28).ok_or_else(bad)? as usize;
    let compression = u32_at(30).ok_or_else(bad)?;
    if w <= 0 || h == 0 || !(bpp == 24 || bpp == 32) || !(compression == 0 || (compression == 3 && bpp == 32)) {
        return Err("Only 24 and 32-bit BMPs are supported".into());
    }
    let (w, rows) = (w as usize, h.unsigned_abs() as usize);
    if w as u64 * rows as u64 > MAX_PIXELS {
        return Err(format!("Too big to show ({w} x {rows})"));
    }
    let stride = (w * bpp / 8 + 3) & !3;
    // the offset comes from the file: it must not wrap a 32-bit usize
    if stride.checked_mul(rows).and_then(|n| n.checked_add(start)).is_none_or(|end| end > d.len()) {
        return Err("This BMP is cut short".into());
    }
    let mut px = Vec::with_capacity(w * rows);
    for y in 0..rows {
        // rows go bottom to top unless the height is negative
        let row = if h > 0 { rows - 1 - y } else { y };
        let line = &d[start + row * stride..];
        for x in 0..w {
            let p = &line[x * bpp / 8..];
            px.push(rgb(p[2], p[1], p[0]));
        }
    }
    Ok((Raw { w, h: rows, px }, (w as u32, rows as u32)))
}

/// `raw` fitted to the screen: scaled down by averaging, or up by a whole
/// factor (pixels stay sharp) when it is small
fn fit(raw: Raw) -> (usize, usize, Vec<u32>) {
    let s = (W as f32 / raw.w as f32).min(H as f32 / raw.h as f32);
    if s >= 1.0 {
        let k = s.floor() as usize;
        if k == 1 {
            return (raw.w, raw.h, raw.px);
        }
        let (w, h) = (raw.w * k, raw.h * k);
        let px = (0..w * h).map(|i| raw.px[(i / w / k) * raw.w + (i % w) / k]).collect();
        return (w, h, px);
    }
    let (w, h) = (((raw.w as f32 * s).round() as usize).max(1), ((raw.h as f32 * s).round() as usize).max(1));
    let (sx, sy) = (raw.w as f32 / w as f32, raw.h as f32 / h as f32);
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        let (y0, y1) = ((y as f32 * sy) as usize, (((y + 1) as f32 * sy) as usize).clamp(1, raw.h));
        for x in 0..w {
            let (x0, x1) = ((x as f32 * sx) as usize, (((x + 1) as f32 * sx) as usize).clamp(1, raw.w));
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for yy in y0..y1.max(y0 + 1) {
                for xx in x0..x1.max(x0 + 1) {
                    let p = raw.px[yy * raw.w + xx];
                    (r, g, b, n) = (r + (p >> 16), g + ((p >> 8) & 255), b + (p & 255), n + 1);
                }
            }
            px.push((r / n) << 16 | (g / n) << 8 | b / n);
        }
    }
    (w, h, px)
}

pub fn load(path: &str) -> Result<Picture, String> {
    let data = read(path)?;
    let (raw, full) = if data.starts_with(b"\x89PNG") {
        png(data)?
    } else if data.starts_with(&[0xFF, 0xD8]) {
        jpeg(data)?
    } else if data.starts_with(b"BM") {
        bmp(data)?
    } else {
        return Err("Not an image Cement can show".into());
    };
    let (w, h, px) = fit(raw);
    Ok(Picture { w, h, px, full })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting() {
        let (w, h, _) = fit(Raw { w: 400, h: 240, px: vec![0; 400 * 240] });
        assert_eq!((w, h), (400, 240), "a Luma screenshot stays 1:1");
        let (w, h, px) = fit(Raw { w: 100, h: 50, px: vec![7; 100 * 50] });
        assert_eq!((w, h, px.len(), px[0]), (400, 200, 400 * 200, 7), "small: a whole factor up");
        let (w, h, px) = fit(Raw { w: 1600, h: 1200, px: vec![0x102030; 1600 * 1200] });
        assert_eq!((w, h, px[0]), (320, 240, 0x102030), "big: down, averaged");
    }

    #[test]
    fn bmp_rows() {
        // 2 x 2, 24 bits, bottom-up: blue red / white black
        let mut d = vec![0u8; 54];
        d[0..2].copy_from_slice(b"BM");
        d[10] = 54;
        d[18] = 2;
        d[22] = 2;
        d[28] = 24;
        d.extend_from_slice(&[255, 255, 255, 0, 0, 0, 0, 0]); // bottom row, padded to 8
        d.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0]); // top row
        let (raw, full) = bmp(d.clone()).unwrap();
        assert_eq!(full, (2, 2));
        assert_eq!(raw.px, vec![0x0000ff, 0xff0000, 0xffffff, 0x000000]);
        d[10..14].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // wraps a 32-bit offset
        assert!(bmp(d).is_err());
    }

    #[test]
    fn jpeg_sizes() {
        // a header alone: SOF2 (progressive) or SOF0, 3 components, then DQT and DHT never come
        let header = |sof: u8, w: u16, h: u16| {
            let mut d = vec![0xFF, 0xD8, 0xFF, sof, 0, 17, 8];
            d.extend_from_slice(&h.to_be_bytes());
            d.extend_from_slice(&w.to_be_bytes());
            d.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xFF, 0xD9]);
            d
        };
        assert_eq!(jpeg(header(0xC2, 4000, 3000)).err().unwrap(), "Too big to show (4000 x 3000)");
        assert_eq!(jpeg(header(0xC0, 65000, 65000)).err().unwrap(), "Too big to show (65000 x 65000)");
        assert_ne!(jpeg(header(0xC0, 4000, 3000)).err().unwrap(), "Too big to show (4000 x 3000)");
    }

    #[test]
    fn png_pixels() {
        // 2 x 1 RGBA, then 2 x 1 grey with 16 bits a sample
        let mut d = Vec::new();
        let mut e = png::Encoder::new(&mut d, 2, 1);
        e.set_color(png::ColorType::Rgba);
        e.write_header().unwrap().write_image_data(&[255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let (raw, full) = png(d).unwrap();
        assert_eq!(full, (2, 1));
        assert_eq!(raw.px, vec![0xff0000, 0x0000ff]);

        let mut d = Vec::new();
        let mut e = png::Encoder::new(&mut d, 2, 1);
        e.set_color(png::ColorType::Grayscale);
        e.set_depth(png::BitDepth::Sixteen);
        e.write_header().unwrap().write_image_data(&[0x80, 0, 0xff, 0xff]).unwrap();
        assert_eq!(png(d).unwrap().0.px, vec![0x808080, 0xffffff]);
        assert!(png(b"not a png".to_vec()).is_err());
    }
}
