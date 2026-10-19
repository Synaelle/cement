// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Cement Text: both screens as console text, in soft reds: the highlight and
// soft colours of the drawn screens' palette (so they follow the accent too),
// install results in soft green or red.

use super::palette::Palette;
use super::{Bottom, Top};
use crate::server::{CTRL_PORT, Mode, progress_of};
use crate::sys::{self, Screen};
use crate::util::{base_name, clip, format_size, ip_text};
use crate::{Server, VERSION, battery_text};

const GOOD: u32 = 0x8ccf9a;
const BAD: u32 = 0xe5484d;

/// a truecolor foreground escape (the console keeps it as RGB565)
fn fg(c: u32) -> String {
    format!("\x1b[38;2;{};{};{}m", c >> 16, (c >> 8) & 255, c & 255)
}

/// the bottom screen is only lit while it lists CIA files (or an install message)
pub const BOTTOM_ALWAYS_ON: bool = false;

/// the screens to clear before their next text
pub struct Ui {
    clear_top: bool,
    clear_bottom: bool,
}

impl Ui {
    pub fn new() -> Ui {
        Ui { clear_top: false, clear_bottom: false }
    }

    /// after the keyboard, the HOME menu or sleep: rows that aren't printed
    /// every time may hold something else, so both consoles start blank
    pub fn invalidate(&mut self) {
        (self.clear_top, self.clear_bottom) = (true, true);
    }
}

impl Server {
    pub(super) fn draw_bottom(&mut self, b: &Bottom) {
        let (show_msg, busy) = (b.show_msg, b.busy);
        let pal = Palette::for_accent(self.accent);
        let hi = fg(pal.highlight); // titles and highlights
        let mut t = String::new();
        if std::mem::take(&mut self.ui.clear_bottom) {
            t += "\x1b[2J";
        }
        t += &format!("\x1b[1;1H{hi} CIAs sent since start, or in /, /cias\x1b[0m\x1b[K\n");
        t += " --------------------------------------\x1b[K\n";
        const ROWS: usize = 20;
        let first = if self.cia_sel >= ROWS { self.cia_sel - ROWS + 1 } else { 0 };
        for row in 0..ROWS {
            let Some(cia) = self.cias.get(first + row) else {
                t += "\x1b[K\n";
                continue;
            };
            let sel = first + row == self.cia_sel;
            t += &format!(
                "{}{} {:<27} {:>9}\x1b[0m\x1b[K\n",
                if sel { hi.as_str() } else { "" },
                if sel { '>' } else { ' ' },
                clip(base_name(&cia.path), 27),
                format_size(cia.size)
            );
        }

        t += "\x1b[K\n";
        if let Some(inst) = &self.inst {
            let done = *inst.shared.done.lock().unwrap();
            t += &format!(" Installing {}\x1b[K\n", clip(base_name(&inst.path), 27));
            if inst.shared.preparing.load(std::sync::atomic::Ordering::Relaxed) {
                t += "   Preparing... removing the old copy\x1b[K\n";
            } else {
                t += &format!(
                    "   {} / {}  {}%\x1b[K\n",
                    format_size(done),
                    format_size(inst.total),
                    if inst.total != 0 { done * 100 / inst.total } else { 0 }
                );
            }
        } else {
            t += "\x1b[K\n\x1b[K\n";
        }
        let color = if !show_msg {
            String::new()
        } else if super::good_news(&self.cia_msg) {
            fg(GOOD)
        } else {
            fg(BAD)
        };
        t += &format!(" {}{}\x1b[0m\x1b[K\n", color, if show_msg { clip(&self.cia_msg, 38) } else { "" });
        if busy && !self.installing() {
            t += &format!("\x1b[30;1H{hi} A: install (waits for transfers)\x1b[0m\x1b[K");
        } else {
            t += "\x1b[30;1H A: install + delete   Up/Down: select\x1b[K";
        }
        t += "\x1b[29;1H Y: FTP login (user and password)\x1b[K";
        sys::select(Screen::Bottom);
        sys::print(&t);
        sys::select(Screen::Top);
    }

    pub(super) fn draw_top(&mut self, top: &Top) {
        let now = sys::now_ms();
        let (bat, low, active) = (top.bat, top.low, top.active);
        let pal = Palette::for_accent(self.accent);
        let (hi, soft) = (fg(pal.highlight), fg(pal.soft)); // titles and highlights, the address

        // every line on a fixed row, cleared to the end and kept under the 50
        // column width (a wrapped line used to leave old text behind)
        let mut t = String::new();
        if std::mem::take(&mut self.ui.clear_top) {
            t += "\x1b[2J";
        }
        t += &format!("\x1b[1;1H{hi} Cement Text\x1b[0m v{VERSION}\x1b[K");
        t += "\x1b[2;1H ------------------------------------------------\x1b[K";
        if self.net.ip != 0 {
            t += &format!("\x1b[4;1H IP   : {soft}{}\x1b[0m\x1b[K", ip_text(self.net.ip));
        } else {
            t += "\x1b[4;1H IP   : -\x1b[K";
        }
        t += &format!("\x1b[5;1H Port : {soft}{CTRL_PORT}\x1b[0m\x1b[K");
        match &self.login {
            Some((user, _)) => t += &format!("\x1b[6;1H Login: {soft}{}\x1b[0m\x1b[K", clip(user, 40)),
            None => t += "\x1b[6;1H Login: none, anyone can connect\x1b[K",
        }
        t += &format!("\x1b[7;1H Status: {}\x1b[K", clip(&self.status, 40));
        t += &format!(
            "\x1b[8;1H Battery: {}{}\x1b[0m{}\x1b[K",
            if low { hi.as_str() } else { "" },
            battery_text(bat),
            if bat.charging {
                "  charging"
            } else if bat.plugged {
                "  plugged in"
            } else {
                ""
            }
        );
        if low {
            t += &format!("\x1b[13;1H{hi} Battery low: new transfers and installs are\x1b[K\x1b[14;1H refused until the charger is plugged in\x1b[0m\x1b[K");
        } else {
            t += "\x1b[13;1H\x1b[K\x1b[14;1H\x1b[K";
        }

        if let Some(i) = active {
            let s = &self.sess[i];
            t += &format!(
                "\x1b[9;1H {} {}\x1b[K",
                if s.mode == Mode::Stor { "Receiving" } else { "Sending  " },
                clip(&s.name, 36)
            );
            if s.mode == Mode::Retr {
                t += &format!(
                    "\x1b[10;1H   {} / {}   {}/s\x1b[K",
                    format_size(s.x.done),
                    format_size(s.x.size),
                    format_size(self.rate)
                );
            } else {
                t += &format!("\x1b[10;1H   {}   {}/s\x1b[K", format_size(s.x.done), format_size(self.rate));
            }
        } else {
            t += "\x1b[9;1H\x1b[K\x1b[10;1H\x1b[K";
        }

        // whole job announced by the PC (SITE BATCH): files left and time left
        if let Some(s) = self.sess.iter().find(|s| s.ctrl.is_some() && s.batch_files != 0) {
            let p = progress_of(s, now);
            let left = if p.batch_eta >= 0 { format!(", {}:{:02} left", p.batch_eta / 60, p.batch_eta % 60) } else { String::new() };
            t += &format!(
                "\x1b[11;1H {} {}/{} files{}\x1b[K",
                match s.batch_kind.as_str() {
                    "delete" => "Deleting",
                    "down" => "Sending  ",
                    _ => "Receiving",
                },
                s.batch_files_done,
                s.batch_files,
                left
            );
        } else {
            t += "\x1b[11;1H\x1b[K";
        }

        t += "\x1b[26;1H START: exit   SELECT: top screen on/off\x1b[K";
        t += "\x1b[27;1H Close the lid to sleep (Wi-Fi turns off)\x1b[K";
        t += "\x1b[28;1H Y: FTP login (user and password)\x1b[K";
        sys::select(Screen::Top);
        sys::print(&t);
    }
}
