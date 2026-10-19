// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Glyph atlases made by build.rs from assets/fonts (Fredoka and Nunito, OFL).

/// one glyph: `w` x `h` coverage bytes at `at` in the bitmap
pub struct G {
    pub ch: u32,
    pub at: u32,
    pub w: u8,
    pub h: u8,
    pub left: i8, // from the pen to the bitmap's left edge
    pub top: i8,  // from the baseline down to the bitmap's top row
    pub adv: u16, // 1/64 px
}

pub struct Font {
    pub ascent: f32, // baseline below the top of the line
    pub height: f32, // CSS "line-height: normal"
    pub glyphs: &'static [G],
    pub kern: &'static [(u16, i16)], // ASCII pairs (a << 8 | b), 1/64 px
    pub bitmap: &'static [u8],
}

/// a button glyph (Kenney's Input Prompts, CC0): one `size` x `size` coverage
/// mask per colour of the drawing, tinted when drawn
pub struct Icon {
    pub size: usize,
    pub layers: &'static [(u32, &'static [u8])],
}

include!(concat!(env!("OUT_DIR"), "/fonts.rs"));

impl Font {
    /// characters missing from the atlas show as '?'
    pub fn glyph(&self, ch: char) -> &G {
        let find = |c: u32| self.glyphs.binary_search_by_key(&c, |g| g.ch).ok();
        let i = find(ch as u32).or_else(|| find('?' as u32)).unwrap_or(0);
        &self.glyphs[i]
    }

    fn kern(&self, a: char, b: char) -> f32 {
        if !a.is_ascii() || !b.is_ascii() {
            return 0.0;
        }
        let key = (a as u16) << 8 | b as u16;
        self.kern.binary_search_by_key(&key, |k| k.0).map_or(0.0, |i| self.kern[i].1 as f32 / 64.0)
    }

    /// pen advance from `prev` (None at the start) to after `ch`
    pub fn step(&self, prev: Option<char>, ch: char, spacing: f32) -> (f32, f32) {
        let k = prev.map_or(0.0, |p| self.kern(p, ch));
        (k, self.glyph(ch).adv as f32 / 64.0 + spacing)
    }

    pub fn width(&self, s: &str, spacing: f32) -> f32 {
        let mut w = 0.0;
        let mut prev = None;
        for ch in s.chars() {
            let (k, adv) = self.step(prev, ch, spacing);
            w += k + adv;
            prev = Some(ch);
        }
        w
    }

    /// `s`, or as much of it as fits in `max` with an ellipsis
    pub fn fit(&self, s: &str, max: f32) -> String {
        if self.width(s, 0.0) <= max {
            return s.to_string();
        }
        let room = max - self.width("…", 0.0);
        let mut w = 0.0;
        let mut prev = None;
        let mut end = 0;
        for (i, ch) in s.char_indices() {
            let (k, adv) = self.step(prev, ch, 0.0);
            if w + k + adv > room {
                break;
            }
            w += k + adv;
            prev = Some(ch);
            end = i + ch.len_utf8();
        }
        format!("{}…", s[..end].trim_end())
    }
}
