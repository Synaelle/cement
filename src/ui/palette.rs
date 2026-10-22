// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The screens' colours: Cement's own graphite and Miku teal for the default
// accent (Baguette keeps its red, which SITE COLOR still reports). Any other
// accent puts each colour's lightness on the accent's hue, the way Baguette's
// applyAccent (app/ui/app.js) does (kept for when the accent is picked again).

use crate::settings::DEFAULT_ACCENT;

pub type Color = u32; // 0xRRGGBB

#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    pub accent: Color,
    pub on_accent: Color, // text on the accent
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

/// the hand-picked palette for the default accent: graphite and Miku teal
const DEFAULT: Palette = Palette {
    accent: 0x3cc8bd,
    on_accent: 0x062320,
    highlight: 0x8ee6de,
    soft: 0xc9cdd3,
    bg: 0x121316,
    blob: 0x191b1f,
    card: 0x1b1d21,
    track: 0x24262b,
    selected: 0x17302f,
    border: 0x2e3238,
    text: 0xf1f2f4,
    dim: 0x9a9fa8,
};

/// (hue in degrees, saturation, lightness), as Baguette's toHsl
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

/// CSS hsl() to RGB, after the rounding Baguette's hsl() string does (hue to
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
            from_hsl(h, s0 * (sat / 0.57).min(1.0), l0)
        };
        let d = DEFAULT;
        Palette {
            accent,
            on_accent: 0xffffff,
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
    fn default_round_trips() {
        // the default colours come back unchanged on their own hue
        for c in [0x3cc8bd, 0x8ee6de, 0x24262b, 0x17302f, 0x2e3238, 0xf1f2f4] {
            let (h, s, l) = to_hsl(c);
            let back = from_hsl(h, s, l);
            let diff = |s: u32| ((back >> s) & 255).abs_diff((c >> s) & 255);
            assert!(diff(16) <= 1 && diff(8) <= 1 && diff(0) <= 1, "{c:06x} -> {back:06x}");
        }
        assert!(Palette::for_accent(DEFAULT_ACCENT) == DEFAULT);
    }
}
