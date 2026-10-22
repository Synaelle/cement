// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// A tiny software canvas: one screen image in 0xRRGGBB, with anti-aliased
// rounded rectangles (one elliptic radius per corner), pills, circles and
// alpha-blended text. Coordinates are in pixels, fractions allowed.

use super::font::{Font, Icon};

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

    /// snapped to whole pixels, so straight edges and borders stay one pixel sharp
    pub fn round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
        let (x, y) = (x.round(), y.round());
        let (w, h) = ((x + w).round() - x, (y + h).round() - y);
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
            // whole pixels only: the atlas advances are whole already
            let gx = pen.round() as i32 + g.left as i32;
            let gy = base + g.top as i32;
            let w = g.w as usize;
            let bits = &f.bitmap[g.at as usize..][..w * g.h as usize];
            for (row, line) in bits.chunks(w.max(1)).enumerate() {
                for (col, &a) in line.iter().enumerate() {
                    self.blend(gx + col as i32, gy + row as i32, c, a as u32);
                }
            }
            pen += adv;
            prev = Some(ch);
        }
        pen
    }

    /// a button glyph at whole pixels; `tint` maps each colour of the drawing
    pub fn icon(&mut self, ic: &Icon, x: f32, y: f32, tint: impl Fn(u32) -> Color) {
        let (x, y) = (x.round() as i32, y.round() as i32);
        for &(color, mask) in ic.layers {
            let c = tint(color);
            for (i, &a) in mask.iter().enumerate() {
                self.blend(x + (i % ic.size) as i32, y + (i / ic.size) as i32, c, a as u32);
            }
        }
    }

    /// a square image of `size` pixels (0xRRGGBB) at whole pixels, its corners rounded by `r`
    pub fn image(&mut self, x: f32, y: f32, size: usize, px: &[u32], r: f32) {
        let (x, y) = (x.round() as i32, y.round() as i32);
        let s = size as f32;
        for (i, &c) in px.iter().enumerate().take(size * size) {
            let (fx, fy) = ((i % size) as f32 + 0.5, (i / size) as f32 + 0.5);
            // distance into the nearest corner's quarter circle, if in one
            let (dx, dy) = ((r - fx).max(fx - (s - r)), (r - fy).max(fy - (s - r)));
            let cover = if dx > 0.0 && dy > 0.0 { ellipse_cover(dx, dy, r, r) } else { 1.0 };
            self.blend(x + (i % size) as i32, y + (i / size) as i32, c, (cover * 255.0 + 0.5) as u32);
        }
    }

    /// a w x h image (0xRRGGBB) at whole pixels, clipped to the canvas
    pub fn blit(&mut self, x: i32, y: i32, w: usize, h: usize, px: &[u32]) {
        for row in 0..h {
            let ty = y + row as i32;
            if ty < 0 || ty as usize >= self.h {
                continue;
            }
            for col in 0..w {
                let tx = x + col as i32;
                if tx >= 0 && (tx as usize) < self.w {
                    self.px[ty as usize * self.w + tx as usize] = px[row * w + col];
                }
            }
        }
    }

    /// right-aligned at `right`
    pub fn text_right(&mut self, f: &Font, right: f32, y: f32, s: &str, c: Color) {
        let w = f.width(s, 0.0);
        self.text(f, right - w, y, s, c, 0.0);
    }
}
