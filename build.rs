// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Rasterises the few font sizes the drawn screens use into glyph atlases, so
// no TTF is ever opened on the 3DS, and the button glyphs (assets/buttons,
// Kenney's SVGs) into coverage masks. Writes $OUT_DIR/fonts.rs (metrics,
// kerning, glyphs) and the bitmaps; src/ui/gfx/font.rs includes them.

use std::fmt::Write as _;
use std::path::PathBuf;

/// (Rust name, font file, size in pixels)
const SIZES: &[(&str, &str, f32)] = &[
    ("FREDOKA_BOLD_22", "fredoka/Fredoka-Bold.ttf", 22.0),
    ("FREDOKA_BOLD_16", "fredoka/Fredoka-Bold.ttf", 16.0),
    ("FREDOKA_SEMI_26", "fredoka/Fredoka-SemiBold.ttf", 26.0),
    // the board's body sizes plus 1 px: smaller is hard to read on the real LCDs
    ("NUNITO_XBOLD_13", "nunito/Nunito-ExtraBold.ttf", 13.0),
    ("NUNITO_XBOLD_12", "nunito/Nunito-ExtraBold.ttf", 12.0),
    ("NUNITO_XBOLD_11", "nunito/Nunito-ExtraBold.ttf", 11.0),
    ("NUNITO_BOLD_13", "nunito/Nunito-Bold.ttf", 13.0),
    ("NUNITO_BOLD_12", "nunito/Nunito-Bold.ttf", 12.0),
    ("NUNITO_BOLD_11", "nunito/Nunito-Bold.ttf", 11.0),
];

/// printable ASCII, Latin-1 and the ellipsis used to shorten long names
fn chars() -> Vec<char> {
    (0x20u32..=0x7E).chain(0xA0..=0xFF).chain([0x2026]).filter_map(char::from_u32).collect()
}

/// unhinted glyphs at 10-26 px have soft grey edges: steepen the coverage so
/// the edge pixels go closer to full or empty
fn crisp(a: u8) -> u8 {
    let t = a as f32 / 255.0;
    let t = ((t - 0.5) * CONTRAST + 0.5).clamp(0.0, 1.0).powf(GAMMA);
    (t * 255.0 + 0.5) as u8
}
const CONTRAST: f32 = 1.4;
const GAMMA: f32 = 0.9;

/// the size near `px` that puts the top of the x on a whole pixel (the
/// baseline is one already), so the flat tops and bottoms of the letters are
/// sharp instead of spread over two rows
fn snapped(font: &fontdue::Font, px: f32) -> f32 {
    let b = font.metrics('x', px).bounds;
    let xh = b.ymin + b.height;
    px * xh.round() / xh
}

/// button glyphs: (Rust name, SVG in assets/buttons, size in pixels). Kenney's
/// 64 x 64 drawings use the square 8..56, which becomes the glyph.
const ICONS: &[(&str, &str, usize)] = &[
    ("BTN_A", "switch_button_a.svg", 15),
    ("BTN_DPAD", "switch_dpad_vertical.svg", 17),
    ("BTN_Y", "switch_button_y.svg", 15),
];

/// the filled paths of a Kenney SVG: (fill colour, outlines as closed polygons)
fn svg_paths(svg: &str) -> Vec<(u32, Vec<Vec<(f32, f32)>>)> {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        Some(tag[at..at + tag[at..].find('"')?].to_string())
    };
    let mut out = Vec::new();
    for tag in svg.split("<path").skip(1) {
        let tag = &tag[..tag.find("/>").unwrap()];
        let fill = u32::from_str_radix(attr(tag, "fill").unwrap().trim_start_matches('#'), 16).unwrap();
        let d = attr(tag, "d").unwrap();
        // tokens: commands and numbers (absolute M, L and Q only, as Kenney writes them)
        let mut toks = Vec::new();
        let mut num = String::new();
        for c in d.chars().chain([' ']) {
            if c.is_ascii_digit() || c == '.' || (c == '-' && num.is_empty()) {
                num.push(c);
                continue;
            }
            if !num.is_empty() {
                toks.push(num.clone());
                num.clear();
            }
            if c == '-' {
                num.push(c);
            } else if c.is_ascii_alphabetic() {
                assert!("MLQZ".contains(c), "unsupported SVG command {c}");
                toks.push(c.to_string());
            }
        }
        let mut polys: Vec<Vec<(f32, f32)>> = Vec::new();
        let (mut cmd, mut i, mut cur) = ('M', 0, (0.0f32, 0.0f32));
        let n = |i: usize| toks[i].parse::<f32>().unwrap();
        while i < toks.len() {
            if let Some(c) = toks[i].chars().next().filter(|c| c.is_ascii_alphabetic()) {
                cmd = c;
                i += 1;
                continue;
            }
            match cmd {
                'M' => {
                    cur = (n(i), n(i + 1));
                    polys.push(vec![cur]);
                    cmd = 'L'; // more pairs after a move are lines
                    i += 2;
                }
                'L' => {
                    cur = (n(i), n(i + 1));
                    polys.last_mut().unwrap().push(cur);
                    i += 2;
                }
                'Q' => {
                    let (c1, end) = ((n(i), n(i + 1)), (n(i + 2), n(i + 3)));
                    for k in 1..=8 {
                        let t = k as f32 / 8.0;
                        let u = 1.0 - t;
                        let p = (
                            u * u * cur.0 + 2.0 * u * t * c1.0 + t * t * end.0,
                            u * u * cur.1 + 2.0 * u * t * c1.1 + t * t * end.1,
                        );
                        polys.last_mut().unwrap().push(p);
                    }
                    cur = end;
                    i += 4;
                }
                _ => i += 1,
            }
        }
        out.push((fill, polys));
    }
    out
}

/// coverage of polygons (non-zero winding) on a size x size grid: 16 rows per
/// pixel and exact span lengths across, so the edges are smooth but not soft
fn fill(polys: &[Vec<(f32, f32)>], size: usize, from: f32, span: f32) -> Vec<u8> {
    const SUB: usize = 16;
    let scale = size as f32 / span;
    let pts: Vec<Vec<(f32, f32)>> =
        polys.iter().map(|p| p.iter().map(|&(x, y)| ((x - from) * scale, (y - from) * scale)).collect()).collect();
    let mut acc = vec![0f32; size * size];
    for row in 0..size * SUB {
        let y = (row as f32 + 0.5) / SUB as f32;
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for p in &pts {
            for k in 0..p.len() {
                let (a, b) = (p[k], p[(k + 1) % p.len()]);
                if (a.1 <= y) != (b.1 <= y) {
                    let x = a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0);
                    xs.push((x, if b.1 > a.1 { 1 } else { -1 }));
                }
            }
        }
        xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let (mut wind, mut start) = (0, 0.0);
        for &(x, dir) in &xs {
            let before = wind;
            wind += dir;
            if before == 0 {
                start = x;
            } else if wind == 0 {
                let (mut a, b) = (start.max(0.0), x.min(size as f32));
                while a < b {
                    let px = a.floor();
                    let end = (px + 1.0).min(b);
                    acc[row / SUB * size + px as usize] += (end - a) / SUB as f32;
                    a = end;
                }
            }
        }
    }
    acc.iter().map(|&a| (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect()
}

/// the button glyphs, one coverage mask per fill colour (tinted when drawn)
fn icons(rs: &mut String, out: &std::path::Path) {
    println!("cargo:rerun-if-changed=assets/buttons");
    for &(name, file, size) in ICONS {
        let svg = std::fs::read_to_string(format!("assets/buttons/{file}")).unwrap();
        let mut layers = String::new();
        for (k, (color, polys)) in svg_paths(&svg).into_iter().enumerate() {
            let bin = format!("{}_{k}.bin", name.to_lowercase());
            std::fs::write(out.join(&bin), fill(&polys, size, 8.0, 48.0)).unwrap();
            write!(layers, "({color:#08x}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{bin}\"))),").unwrap();
        }
        writeln!(rs, "pub static {name}: Icon = Icon {{ size: {size}, layers: &[{layers}] }};").unwrap();
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/fonts");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut rs = String::new();

    let chars = chars();
    for &(name, file, px) in SIZES {
        let data = std::fs::read(format!("assets/fonts/{file}")).unwrap();
        let font = fontdue::Font::from_bytes(data, fontdue::FontSettings { scale: px, ..Default::default() }).unwrap();
        let line = font.horizontal_line_metrics(px).unwrap(); // the layout keeps the nominal size
        let size = snapped(&font, px);

        let mut bitmap = Vec::new();
        let mut glyphs = String::new();
        for &ch in &chars {
            if font.lookup_glyph_index(ch) == 0 && ch != ' ' {
                continue; // drawn as '?'
            }
            let (m, cov) = font.rasterize(ch, size);
            let top = -(m.ymin + m.height as i32);
            write!(
                glyphs,
                "G{{ch:{},at:{},w:{},h:{},left:{},top:{},adv:{}}},",
                ch as u32,
                bitmap.len(),
                m.width,
                m.height,
                m.xmin,
                top,
                m.advance_width.round() as i32 * 64 // whole pixels, as hinted fonts do
            )
            .unwrap();
            bitmap.extend(cov.into_iter().map(crisp));
        }

        let mut kern = String::new();
        for a in ' '..='~' {
            for b in ' '..='~' {
                if let Some(k) = font.horizontal_kern(a, b, size) {
                    let k = k.round() as i32 * 64;
                    if k != 0 {
                        write!(kern, "({},{k}),", (a as u32) << 8 | b as u32).unwrap();
                    }
                }
            }
        }

        let bin = format!("{}.bin", name.to_lowercase());
        std::fs::write(out.join(&bin), &bitmap).unwrap();
        writeln!(
            rs,
            "pub static {name}: Font = Font {{ ascent: {:?}, height: {:?}, glyphs: &[{glyphs}], kern: &[{kern}], \
             bitmap: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{bin}\")) }};",
            line.ascent,
            line.ascent - line.descent + line.line_gap,
        )
        .unwrap();
    }
    icons(&mut rs, &out);
    std::fs::write(out.join("fonts.rs"), rs).unwrap();
}
