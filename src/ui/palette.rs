// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: AGPL-3.0-or-later
// The screens' colours, picked in Settings: Cement's warm black and red, or
// graphite and Miku teal. The red look follows the accent (Settings' Colour
// row, or SITE COLOR from Baguette), the same way Baguette does: each colour
// takes the accent's hue, and a lightness that keeps its luminance, so text
// stays as easy to read as on the default red. Miku keeps its own colours.

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

/// the colours the Settings tab steps through, the same as Baguette's, each as
/// light as the default red (Custom is any other colour Baguette set)
pub const PRESETS: [(&str, Color); 8] = [
    ("Red", DEFAULT_ACCENT),
    ("Orange", 0x9b4a22),
    ("Amber", 0x895615),
    ("Green", 0x2d6e40),
    ("Teal", 0x186d69),
    ("Blue", 0x2960b3),
    ("Violet", 0x7546c2),
    ("Pink", 0xa63381),
];

/// the looks the Settings tab offers
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Theme {
    Red,  // Cement's own: warm black and red
    Miku, // graphite and Miku teal
}

pub const THEMES: [Theme; 2] = [Theme::Red, Theme::Miku];

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Theme::Miku => "Miku",
            Theme::Red => "Red",
        }
    }

    /// its value in settings.ini
    pub fn key(self) -> &'static str {
        match self {
            Theme::Miku => "miku",
            Theme::Red => "red",
        }
    }

    fn palette(self) -> Palette {
        match self {
            Theme::Miku => MIKU,
            Theme::Red => RED,
        }
    }
}

/// graphite and Miku teal
const MIKU: Palette = Palette {
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

/// warm black and red, Baguette's colours too
const RED: Palette = Palette {
    accent: DEFAULT_ACCENT,
    on_accent: 0xffffff,
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

/// HSL (hue in degrees, saturation and lightness 0 to 1) to RGB
fn from_hsl(h: f64, s: f64, l: f64) -> Color {
    let h = h.rem_euclid(360.0);
    let a = s * l.min(1.0 - l);
    let f = |n: f64| {
        let k = (n + h / 30.0) % 12.0;
        let v = l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0);
        ((v * 255.0).round() as u32).min(255)
    };
    f(0.0) << 16 | f(8.0) << 8 | f(4.0)
}

/// WCAG relative luminance
fn luminance(c: Color) -> f64 {
    let lin = |v: u32| {
        let v = (v & 255) as f64 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c >> 16) + 0.7152 * lin(c >> 8) + 0.0722 * lin(c)
}

/// the colour of hue h and saturation s whose luminance is `target` (lightness found by bisection)
fn with_luminance(h: f64, s: f64, target: f64) -> Color {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if luminance(from_hsl(h, s, mid)) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    from_hsl(h, s, (lo + hi) / 2.0)
}

impl Palette {
    /// the look's palette; the red one on another accent than the default
    /// takes the accent's hue, as light (in luminance) as the default's colours
    pub fn for_accent(theme: Theme, accent: Color) -> Palette {
        let d = theme.palette();
        if theme == Theme::Miku || accent == DEFAULT_ACCENT {
            return d;
        }
        let (h, sat, _) = to_hsl(accent);
        let base = to_hsl(DEFAULT_ACCENT).1;
        let follow = |c: Color| {
            let (_, s0, _) = to_hsl(c);
            // a greyer accent gives greyer surfaces
            with_luminance(h, s0 * (sat / base).min(1.0), luminance(c))
        };
        Palette {
            accent: with_luminance(h, sat, luminance(d.accent)),
            on_accent: d.on_accent,
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
        for c in [0x3cc8bd, 0x8ee6de, 0x24262b, 0x17302f, 0x2e3238, 0xf1f2f4, 0xd8676a, 0xe9a9a3, 0x2a1516, 0x3d1418, 0x4d1d21, 0xf2e7e4] {
            let (h, s, l) = to_hsl(c);
            let back = from_hsl(h, s, l);
            let diff = |s: u32| ((back >> s) & 255).abs_diff((c >> s) & 255);
            assert!(diff(16) <= 1 && diff(8) <= 1 && diff(0) <= 1, "{c:06x} -> {back:06x}");
        }
        for t in THEMES {
            assert!(Palette::for_accent(t, DEFAULT_ACCENT) == t.palette());
        }
    }

    #[test]
    fn accents_keep_the_contrast() {
        // every preset's colours are as light as the default red's, and Miku ignores the accent
        let red = Palette::for_accent(Theme::Red, DEFAULT_ACCENT);
        for (name, c) in PRESETS {
            let p = Palette::for_accent(Theme::Red, c);
            let pairs = [(p.accent, red.accent), (p.text, red.text), (p.dim, red.dim), (p.bg, red.bg), (p.card, red.card), (p.highlight, red.highlight)];
            for (got, want) in pairs {
                let (a, b) = (luminance(got), luminance(want));
                assert!((a - b).abs() <= 0.01 + 0.03 * b, "{name}: {got:06x} vs {want:06x}");
            }
            assert!(Palette::for_accent(Theme::Miku, c) == Palette::for_accent(Theme::Miku, DEFAULT_ACCENT));
        }
    }
}
