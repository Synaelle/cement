// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The SD card paths the Browse tab won't delete while "Protect important 3DS
// files" is on, the same as Baguette's (core/src/protect.rs): losing them can
// wipe installed games and saves, or stop the console from booting into its
// custom firmware. FTP clients aren't limited by it (Baguette checks on its side).

/// the protected paths, and what they hold (short, for the bottom screen)
pub const PATHS: &[(&str, &str)] = &[
    ("/Nintendo 3DS", "games and saves"),
    ("/boot.firm", "what the 3DS boots"),
    ("/luma", "Luma3DS settings"),
    ("/gm9", "GodMode9 files"),
    ("/boot.3dsx", "the Homebrew Launcher"),
];

/// inside a protected folder but still free: what Luma3DS writes for you to take
const FREE: &[&str] = &["/luma/screenshots", "/luma/dumps"];

fn lower(p: &str) -> String {
    p.trim_end_matches('/').to_ascii_lowercase()
}

/// a is b or inside it
fn within(a: &str, b: &str) -> bool {
    let (a, b) = (lower(a), lower(b));
    a == b || a.starts_with(&format!("{b}/"))
}

/// Some(why) when deleting this path would hit a protected one (the path
/// itself, something inside one, or a folder holding one)
pub fn delete(path: &str) -> Option<String> {
    let path = if path.is_empty() { "/" } else { path };
    let free = FREE.iter().any(|f| within(path, f));
    PATHS
        .iter()
        .find(|(p, _)| (within(path, p) && !free) || within(p, path))
        .map(|(p, what)| format!("{p} is protected: {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert!(delete("/Nintendo 3DS").is_some());
        assert!(delete("/nintendo 3ds/abc/title").is_some(), "inside, any case");
        assert!(delete("/").is_some(), "the root holds them all");
        assert!(delete("/boot.firm").is_some());
        assert!(delete("/luma/payloads/old.firm").is_some(), "inside too");
        assert!(delete("/luma/config.ini").is_some());
        assert!(delete("/3ds").is_none(), "homebrew lives there: not protected");
        assert!(delete("/3ds/app").is_none());
        assert!(delete("/Nintendo 3DS games").is_none(), "only the exact name");
        assert!(delete("/notes.txt").is_none());
        assert!(delete("/luma/screenshots").is_none(), "Luma's screenshots stay free");
        assert!(delete("/luma/screenshots/top.bmp").is_none());
        assert!(delete("/luma/dumps/arm11/crash.dmp").is_none(), "and its crash dumps");
    }
}
