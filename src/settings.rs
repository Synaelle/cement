// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Settings kept on the SD card in sdmc:/3ds/cement/settings.ini, one
// "key=value" per line: the accent colour (SITE COLOR) and the FTP login
// (SITE LOGIN, or Y on the bottom screen). The password is stored as plain text.

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

/// a user name for the login: not empty, no spaces or control characters
pub fn valid_user(user: &str) -> bool {
    !user.is_empty() && user.len() <= 64 && user.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

fn read() -> String {
    let Some(f) = File::open_read(FILE) else { return String::new() };
    let size = f.size().unwrap_or(0).min(4096) as usize;
    let mut buf = vec![0; size];
    let n = f.read_at(0, &mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// the value of `key`, as written (a password keeps its spaces)
fn get(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim_end_matches('\r');
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.to_string())
    })
}

/// sets (Some) or removes (None) keys, keeping every other line
fn set(changes: &[(&str, Option<&str>)]) -> bool {
    let mut text: String = read()
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !changes.iter().any(|(k, _)| l.split_once('=').is_some_and(|(lk, _)| lk.trim() == *k)))
        .map(|l| format!("{l}\n"))
        .collect();
    for (k, v) in changes {
        if let Some(v) = v {
            text += &format!("{k}={v}\n");
        }
    }
    sys::create_dir("/3ds"); // fails when they exist already
    sys::create_dir(DIR);
    let Some(f) = File::open_write(FILE) else { return false };
    let ok = f.write_at(0, text.as_bytes()) == Some(text.len()) && f.set_size(text.len() as u64);
    f.flush();
    ok
}

pub struct Settings {
    pub accent: u32,
    pub login: Option<(String, String)>, // user and password; None: anonymous
}

pub fn load() -> Settings {
    let text = read();
    let accent = get(&text, "accent").and_then(|v| parse_color(&v)).unwrap_or(DEFAULT_ACCENT);
    let login = match (get(&text, "user"), get(&text, "pass")) {
        (Some(u), p) if valid_user(&u) => Some((u, p.unwrap_or_default())),
        _ => None,
    };
    Settings { accent, login }
}

pub fn save_accent(accent: u32) -> bool {
    set(&[("accent", Some(&format!("{accent:06x}")))])
}

pub fn save_login(login: Option<(&str, &str)>) -> bool {
    match login {
        Some((u, p)) => set(&[("user", Some(u)), ("pass", Some(p))]),
        None => set(&[("user", None), ("pass", None)]),
    }
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

    #[test]
    fn values() {
        let text = "accent=b8283c\r\nuser=syna\npass= two words \n";
        assert_eq!(get(text, "user").as_deref(), Some("syna"));
        assert_eq!(get(text, "pass").as_deref(), Some(" two words "));
        assert_eq!(get(text, "nope"), None);
        assert!(valid_user("syna") && !valid_user("") && !valid_user("a b"));
    }
}
