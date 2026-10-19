// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Rasterises the few font sizes the drawn screens use into glyph atlases, so
// no TTF is ever opened on the 3DS. Writes $OUT_DIR/fonts.rs (metrics, kerning)
// and one coverage bitmap per size; src/ui/gfx/font.rs includes them.
// Cement Text (feature text-ui) draws no fonts and skips all of this.

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

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/fonts");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut rs = String::new();
    if std::env::var_os("CARGO_FEATURE_TEXT_UI").is_some() {
        std::fs::write(out.join("fonts.rs"), rs).unwrap();
        return;
    }

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
    std::fs::write(out.join("fonts.rs"), rs).unwrap();
}
