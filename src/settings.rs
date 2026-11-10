// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: AGPL-3.0-or-later
// Settings kept on the SD card in sdmc:/3ds/cement/settings.ini, one
// "key=value" per line: the accent colour and when it was picked (Settings or
// SITE COLOR), the FTP login (SITE LOGIN, or Settings) and the other choices of the
// Settings tab. The password isn't stored: only a salted, repeated SHA-256 of
// it (pass_hash), enough to check a login but not to read it back.

use crate::hash::{hex, sha256};
use crate::sys::{self, File};
use crate::ui::palette::{THEMES, Theme};

pub const DEFAULT_ACCENT: u32 = 0xe44c3e;
/// the old default red: read as today's, so it moves to the new one
const OLD_DEFAULT: u32 = 0xb8283c;

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

/// rounds of SHA-256: slows down guessing from a copied settings.ini (about
/// 50 ms on the 3DS)
const ROUNDS: u32 = 1000;

fn stretch(salt: &str, pass: &str, rounds: u32) -> String {
    let mut h = sha256(format!("{salt}:{pass}").as_bytes());
    for _ in 1..rounds {
        h = sha256(&h);
    }
    hex(&h)
}

/// "sha256:<rounds>:<salt>:<hash>" for a password, with a new salt
pub fn hash_pass(pass: &str) -> String {
    // a salt only has to differ between consoles and logins, not be secret
    let seed = format!("{}:{}:{:p}", sys::now_ms(), pass.len(), &pass);
    let salt = hex(&sha256(seed.as_bytes())[..8]);
    format!("sha256:{ROUNDS}:{salt}:{}", stretch(&salt, pass, ROUNDS))
}

/// whether `pass` is the password `stored` (from hash_pass) was made from,
/// with the rounds it was made with, so a change of ROUNDS keeps old logins
pub fn check_pass(stored: &str, pass: &str) -> bool {
    let mut parts = stored.splitn(4, ':');
    match (parts.next(), parts.next().map(str::parse::<u32>), parts.next(), parts.next()) {
        (Some("sha256"), Some(Ok(rounds @ 1..=100_000)), Some(salt), Some(hash)) => stretch(salt, pass, rounds) == hash,
        _ => false,
    }
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
    f.flush() && ok
}

pub struct Settings {
    pub accent: u32,
    pub accent_time: u64, // when the accent was picked, seconds since 1970 on the local clock; 0 = never
    pub login: Option<(String, String)>, // user and password hash; None: anonymous
    pub prefs: Prefs,
}

/// the switches of the Settings tab
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Prefs {
    pub ftp: bool,         // the FTP server runs
    pub screen_idle: bool, // the screens turn off after an idle minute
    pub auto_off: u32,     // minutes without use before the console turns off, 0 = never
    pub cia_info: bool,    // the selected CIA's details on the top screen
    pub protect: bool,     // Browse won't delete the important 3DS files (protect.rs)
    pub update: bool,      // a newer release is looked for at startup (update.rs)
    pub theme: Theme,      // how the screens look
}

impl Default for Prefs {
    fn default() -> Prefs {
        Prefs {
            ftp: true,
            screen_idle: true,
            auto_off: 0,
            cia_info: true,
            protect: true,
            update: true,
            theme: Theme::Red,
        }
    }
}

/// the auto off choices, in minutes
pub const AUTO_OFF: [u32; 5] = [0, 5, 15, 30, 60];

pub fn load() -> Settings {
    let text = read();
    let accent =
        get(&text, "accent").and_then(|v| parse_color(&v)).filter(|&c| c != OLD_DEFAULT).unwrap_or(DEFAULT_ACCENT);
    let accent_time = get(&text, "accent_time").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let login = match (get(&text, "user"), get(&text, "pass_hash"), get(&text, "pass")) {
        (Some(u), Some(h), _) if valid_user(&u) => Some((u, h)),
        (Some(u), None, p) if valid_user(&u) => {
            // a plain password from before 2.3.0: hashed, and the plain one goes
            let h = hash_pass(&p.unwrap_or_default());
            save_login(Some((&u, &h)));
            Some((u, h))
        }
        _ => None,
    };
    let flag = |key: &str, default: bool| get(&text, key).map_or(default, |v| v.trim() != "0");
    let d = Prefs::default();
    let auto_off = get(&text, "auto_off").and_then(|v| v.trim().parse().ok()).filter(|m| AUTO_OFF.contains(m));
    let prefs = Prefs {
        ftp: flag("ftp", d.ftp),
        screen_idle: flag("screen_idle", d.screen_idle),
        auto_off: auto_off.unwrap_or(d.auto_off),
        cia_info: flag("cia_info", d.cia_info),
        protect: flag("protect", d.protect),
        update: flag("update", d.update),
        theme: get(&text, "look").and_then(|v| THEMES.into_iter().find(|t| t.key() == v.trim())).unwrap_or(d.theme),
    };
    Settings { accent, accent_time, login, prefs }
}

pub fn save_prefs(p: &Prefs) -> bool {
    let b = |v: bool| if v { "1" } else { "0" };
    let auto_off = p.auto_off.to_string();
    set(&[
        ("ftp", Some(b(p.ftp))),
        ("screen_idle", Some(b(p.screen_idle))),
        ("auto_off", Some(&auto_off)),
        ("cia_info", Some(b(p.cia_info))),
        ("protect", Some(b(p.protect))),
        ("update", Some(b(p.update))),
        ("look", Some(p.theme.key())),
    ])
}

pub fn save_accent(accent: u32, time: u64) -> bool {
    set(&[("accent", Some(&format!("{accent:06x}"))), ("accent_time", Some(&time.to_string()))])
}

/// the user name and the password hash (hash_pass)
pub fn save_login(login: Option<(&str, &str)>) -> bool {
    match login {
        Some((u, h)) => set(&[("user", Some(u)), ("pass_hash", Some(h)), ("pass", None)]),
        None => set(&[("user", None), ("pass_hash", None), ("pass", None)]),
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

    #[test]
    fn passwords() {
        let h = hash_pass(" two words ");
        assert!(h.starts_with("sha256:1000:") && !h.contains("two"));
        assert!(check_pass(&h, " two words "));
        assert!(!check_pass(&h, "two words") && !check_pass(&h, ""));
        assert!(!check_pass("plain", "plain"), "only hashes are accepted");
        let salt = h.split(':').nth(2).unwrap();
        let other = format!("sha256:10:{salt}:{}", stretch(salt, "pw", 10));
        assert!(check_pass(&other, "pw"), "a hash made with other rounds still works");
        assert!(!check_pass("sha256:0:salt:x", "pw") && !check_pass("sha256:-1:salt:x", "pw"));
    }
}
