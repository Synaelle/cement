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
    ("NUNITO_XBOLD_12", "nunito/Nunito-ExtraBold.ttf", 12.0),
    ("NUNITO_XBOLD_11", "nunito/Nunito-ExtraBold.ttf", 11.0),
    ("NUNITO_XBOLD_10", "nunito/Nunito-ExtraBold.ttf", 10.0),
    ("NUNITO_BOLD_12", "nunito/Nunito-Bold.ttf", 12.0),
    ("NUNITO_BOLD_11", "nunito/Nunito-Bold.ttf", 11.0),
    ("NUNITO_BOLD_10", "nunito/Nunito-Bold.ttf", 10.0),
];

/// printable ASCII, Latin-1 and the ellipsis used to shorten long names
fn chars() -> Vec<char> {
    (0x20u32..=0x7E).chain(0xA0..=0xFF).chain([0x2026]).filter_map(char::from_u32).collect()
}

/// light text on a dark background looks thinner than in a browser when blended
/// linearly: lift the coverage a little
fn lift(a: u8) -> u8 {
    ((a as f32 / 255.0).powf(0.8) * 255.0 + 0.5) as u8
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
        let line = font.horizontal_line_metrics(px).unwrap();

        let mut bitmap = Vec::new();
        let mut glyphs = String::new();
        for &ch in &chars {
            if font.lookup_glyph_index(ch) == 0 && ch != ' ' {
                continue; // drawn as '?'
            }
            let (m, cov) = font.rasterize(ch, px);
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
                (m.advance_width * 64.0).round() as i32
            )
            .unwrap();
            bitmap.extend(cov.into_iter().map(lift));
        }

        let mut kern = String::new();
        for a in ' '..='~' {
            for b in ' '..='~' {
                if let Some(k) = font.horizontal_kern(a, b, px) {
                    let k = (k * 64.0).round() as i32;
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
