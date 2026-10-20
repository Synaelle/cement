// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The screens' colours, all following the accent's hue the way Chomp's
// applyAccent (app/ui/app.js) does, so both apps match: the default accent
// keeps the hand-picked palette, any other one puts each colour's lightness on
// the accent's hue.

use crate::settings::DEFAULT_ACCENT;

pub type Color = u32; // 0xRRGGBB

#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    pub accent: Color,
    pub highlight: Color,
    pub soft: Color,
    pub bg: Color,
    pub blob: Color,
    pub card: Color,
    pub track: Color,
    pub selected: Color,
    pub border: Color,
    pub text: Color,
    pub dim: Color,
}

/// the hand-picked palette for the default accent
const DEFAULT: Palette = Palette {
    accent: DEFAULT_ACCENT,
    highlight: 0xd8676a,
    soft: 0xe9a9a3,
    bg: 0x0e0a0a,
    blob: 0x1a1112,
    card: 0x1d1414,
    track: 0x2a1516,
    selected: 0x3d1418,
    border: 0x4d1d21,
    text: 0xf2e7e4,
    dim: 0xa8928e,
};

/// (hue in degrees, saturation, lightness), as Chomp's toHsl
fn to_hsl(c: Color) -> (f64, f64, f64) {
    let (r, g, b) = (((c >> 16) & 255) as f64 / 255.0, ((c >> 8) & 255) as f64 / 255.0, (c & 255) as f64 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

/// CSS hsl() to RGB, after the rounding Chomp's hsl() string does (hue to
/// 0.1 degree, saturation and lightness to 0.1 %)
fn from_hsl(h: f64, s: f64, l: f64) -> Color {
    let tenth = |v: f64| (v * 10.0).round() / 10.0;
    let (h, s, l) = (tenth(h).rem_euclid(360.0), tenth(s * 100.0) / 100.0, tenth(l * 100.0) / 100.0);
    let a = s * l.min(1.0 - l);
    let f = |n: f64| {
        let k = (n + h / 30.0) % 12.0;
        let v = l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0);
        ((v * 255.0).round() as u32).min(255)
    };
    f(0.0) << 16 | f(8.0) << 8 | f(4.0)
}

impl Palette {
    pub fn for_accent(accent: Color) -> Palette {
        if accent == DEFAULT_ACCENT {
            return DEFAULT;
        }
        let (h, sat, _) = to_hsl(accent);
        let follow = |c: Color| {
            let (_, s0, l0) = to_hsl(c);
            // the default lightness on the accent's hue; a greyer accent gives greyer surfaces
            from_hsl(h, s0 * (sat / 0.64).min(1.0), l0)
        };
        let d = DEFAULT;
        Palette {
            accent,
            highlight: follow(d.highlight),
            soft: follow(d.soft),
            bg: follow(d.bg),
            blob: follow(d.blob),
            card: follow(d.card),
            track: follow(d.track),
            selected: follow(d.selected),
            border: follow(d.border),
            text: follow(d.text),
            dim: follow(d.dim),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_red_round_trips() {
        // the default colours come back unchanged on their own hue
        for c in [0xd8676a, 0xe9a9a3, 0x2a1516, 0x3d1418, 0x4d1d21, 0xf2e7e4] {
            let (h, s, l) = to_hsl(c);
            let back = from_hsl(h, s, l);
            let diff = |s: u32| ((back >> s) & 255).abs_diff((c >> s) & 255);
            assert!(diff(16) <= 1 && diff(8) <= 1 && diff(0) <= 1, "{c:06x} -> {back:06x}");
        }
        assert!(Palette::for_accent(DEFAULT_ACCENT) == DEFAULT);
    }
}
