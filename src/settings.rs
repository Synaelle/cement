// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Settings kept on the SD card in sdmc:/3ds/cement/settings.ini, one
// "key=value" per line. Today only the accent colour (set by Chomp with SITE COLOR).

use crate::sys::{self, File};

pub const DEFAULT_ACCENT: u32 = 0xb8283c;

const DIR: &str = "/3ds/cement";
const FILE: &str = "/3ds/cement/settings.ini";

/// "#rrggbb" or "rrggbb", any case
pub fn parse_color(s: &str) -> Option<u32> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if hex.len() != 6 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

fn read() -> String {
    let Some(f) = File::open_read(FILE) else { return String::new() };
    let size = f.size().unwrap_or(0).min(4096) as usize;
    let mut buf = vec![0; size];
    let n = f.read_at(0, &mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// the saved accent, or the default
pub fn load_accent() -> u32 {
    read()
        .lines()
        .find_map(|l| l.trim().strip_prefix("accent=").and_then(parse_color))
        .unwrap_or(DEFAULT_ACCENT)
}

/// rewrites the file with the new accent, keeping any other lines
pub fn save_accent(accent: u32) -> bool {
    let mut text: String = read()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with("accent="))
        .map(|l| format!("{l}\n"))
        .collect();
    text += &format!("accent={accent:06x}\n");
    sys::create_dir("/3ds"); // fails when they exist already
    sys::create_dir(DIR);
    let Some(f) = File::open_write(FILE) else { return false };
    let ok = f.write_at(0, text.as_bytes()) == Some(text.len()) && f.set_size(text.len() as u64);
    f.flush();
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#B8283C"), Some(0xb8283c));
        assert_eq!(parse_color("cc3a48"), Some(0xcc3a48));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#12345g"), None);
        assert_eq!(parse_color(""), None);
    }
}
