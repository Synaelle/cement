// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// A tiny software canvas: one screen image in 0xRRGGBB, with anti-aliased
// rounded rectangles (one elliptic radius per corner), pills, circles and
// alpha-blended text. Coordinates are in pixels, fractions allowed.

use super::font::Font;

pub type Color = u32;

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

fn mix(bg: u32, fg: u32, a: u32) -> u32 {
    let ch = |s: u32| {
        let (b, f) = ((bg >> s) & 0xFF, (fg >> s) & 0xFF);
        ((b * (255 - a) + f * a + 127) / 255) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// coverage of the pixel centred on (dx, dy) from the centre of an ellipse
fn ellipse_cover(dx: f32, dy: f32, rx: f32, ry: f32) -> f32 {
    let (nx, ny) = (dx / rx, dy / ry);
    let f = nx * nx + ny * ny - 1.0;
    let (gx, gy) = (2.0 * dx / (rx * rx), 2.0 * dy / (ry * ry));
    let g = (gx * gx + gy * gy).sqrt();
    if g == 0.0 {
        return 1.0;
    }
    (0.5 - f / g).clamp(0.0, 1.0)
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![0; w * h] }
    }

    pub fn fill(&mut self, c: Color) {
        self.px.fill(c);
    }

    fn blend(&mut self, x: i32, y: i32, c: Color, a: u32) {
        if a == 0 || x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let p = &mut self.px[y as usize * self.w + x as usize];
        *p = if a >= 255 { c } else { mix(*p, c, a) };
    }

    /// rounded rectangle; radii (rx, ry) for the top-left, top-right,
    /// bottom-right and bottom-left corners, like CSS border-radius
    pub fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: [(f32, f32); 4], c: Color) {
        let (x1, y1) = (x + w, y + h);
        let xa = (x.floor() as i32).max(0);
        let ya = (y.floor() as i32).max(0);
        let xb = (x1.ceil() as i32).min(self.w as i32);
        let yb = (y1.ceil() as i32).min(self.h as i32);
        for py in ya..yb {
            let fy = py as f32 + 0.5;
            let cy = (fy - y + 0.5).min(y1 - fy + 0.5).clamp(0.0, 1.0);
            for px in xa..xb {
                let fx = px as f32 + 0.5;
                // the corner ellipse this pixel falls in, if any
                let corner = if fx < x + r[0].0 && fy < y + r[0].1 {
                    Some((x + r[0].0, y + r[0].1, r[0]))
                } else if fx > x1 - r[1].0 && fy < y + r[1].1 {
                    Some((x1 - r[1].0, y + r[1].1, r[1]))
                } else if fx > x1 - r[2].0 && fy > y1 - r[2].1 {
                    Some((x1 - r[2].0, y1 - r[2].1, r[2]))
                } else if fx < x + r[3].0 && fy > y1 - r[3].1 {
                    Some((x + r[3].0, y1 - r[3].1, r[3]))
                } else {
                    None
                };
                let cover = match corner {
                    Some((ex, ey, (rx, ry))) => ellipse_cover(fx - ex, fy - ey, rx, ry),
                    None => cy * (fx - x + 0.5).min(x1 - fx + 0.5).clamp(0.0, 1.0),
                };
                self.blend(px, py, c, (cover * 255.0 + 0.5) as u32);
            }
        }
    }

    pub fn round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
        let r = r.min(w / 2.0).min(h / 2.0);
        self.rrect(x, y, w, h, [(r, r); 4], c);
    }

    /// a rounded box with a 1 px border
    pub fn card(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, fill: Color, border: Color) {
        self.round(x, y, w, h, r, border);
        self.round(x + 1.0, y + 1.0, w - 2.0, h - 2.0, r - 1.0, fill);
    }

    pub fn pill(&mut self, x: f32, y: f32, w: f32, h: f32, c: Color) {
        self.round(x, y, w, h, h / 2.0, c);
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Color) {
        self.round(cx - r, cy - r, 2.0 * r, 2.0 * r, r, c);
    }

    /// `s` with its baseline at `y`; returns the pen position after it
    pub fn text(&mut self, f: &Font, x: f32, y: f32, s: &str, c: Color, spacing: f32) -> f32 {
        let base = y.round() as i32;
        let mut pen = x;
        let mut prev = None;
        for ch in s.chars() {
            let (k, adv) = f.step(prev, ch, spacing);
            pen += k;
            let g = f.glyph(ch);
            // placed at a fraction of a pixel: each column shares its coverage
            // with the next one, so spacing stays even at small sizes
            let start = pen.floor();
            let frac = ((pen - start) * 256.0) as u32;
            let gx = start as i32 + g.left as i32;
            let gy = base + g.top as i32;
            let w = g.w as usize;
            let bits = &f.bitmap[g.at as usize..][..w * g.h as usize];
            for (row, line) in bits.chunks(w.max(1)).enumerate() {
                let mut prev = 0;
                for col in 0..=w {
                    let a = line.get(col).copied().unwrap_or(0) as u32;
                    let cover = (a * (256 - frac) + prev * frac) >> 8;
                    self.blend(gx + col as i32, gy + row as i32, c, cover);
                    prev = a;
                }
            }
            pen += adv;
            prev = Some(ch);
        }
        pen
    }

    /// right-aligned at `right`
    pub fn text_right(&mut self, f: &Font, right: f32, y: f32, s: &str, c: Color) {
        let w = f.width(s, 0.0);
        self.text(f, right - w, y, s, c, 0.0);
    }

    /// a line `width` px thick with round ends
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Color) {
        let r = width / 2.0;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let xa = ((x0.min(x1) - r).floor() as i32).max(0);
        let xb = ((x0.max(x1) + r).ceil() as i32).min(self.w as i32);
        let ya = ((y0.min(y1) - r).floor() as i32).max(0);
        let yb = ((y0.max(y1) + r).ceil() as i32).min(self.h as i32);
        for py in ya..yb {
            for px in xa..xb {
                let (fx, fy) = (px as f32 + 0.5 - x0, py as f32 + 0.5 - y0);
                let t = ((fx * dx + fy * dy) / len2).clamp(0.0, 1.0);
                let (ex, ey) = (fx - t * dx, fy - t * dy);
                let cover = (r + 0.5 - (ex * ex + ey * ey).sqrt()).clamp(0.0, 1.0);
                self.blend(px, py, c, (cover * 255.0 + 0.5) as u32);
            }
        }
    }

    /// a thin arrow `h` px tall pointing up (or down), its foot on the baseline `y`
    pub fn arrow(&mut self, x: f32, y: f32, h: f32, up: bool, c: Color) {
        let (tip, foot) = if up { (y - h, y) } else { (y, y - h) };
        let head = if up { 3.0 } else { -3.0 };
        let cx = x + 3.0;
        self.line(cx, tip, cx, foot, 1.1, c);
        self.line(cx, tip, cx - 2.6, tip + head, 1.1, c);
        self.line(cx, tip, cx + 2.6, tip + head, 1.1, c);
    }
}
