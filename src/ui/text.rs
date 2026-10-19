// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Cement Text: both screens as console text, in red (titles red, highlights
// bright red, install results green or red).

use super::{Bottom, Top};
use crate::server::{CTRL_PORT, Mode, progress_of};
use crate::sys::{self, Screen};
use crate::util::{base_name, clip, format_size, ip_text};
use crate::{Server, VERSION, battery_text};

const TITLE: &str = "\x1b[31m";
const HIGHLIGHT: &str = "\x1b[1;31m";

/// the bottom screen is only lit while it lists CIA files (or an install message)
pub const BOTTOM_ALWAYS_ON: bool = false;

/// the console keeps no state of its own
pub struct Ui;

impl Ui {
    pub fn new() -> Ui {
        Ui
    }

    /// after the HOME menu or sleep: nothing to do, every line is printed again
    pub fn invalidate(&mut self) {}
}

impl Server {
    pub(super) fn draw_bottom(&mut self, b: &Bottom) {
        let (show_msg, busy) = (b.show_msg, b.busy);
        let mut t = String::new();
        t += &format!("\x1b[1;1H{TITLE} CIA files ready to install\x1b[0m\x1b[K\n");
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
                if sel { HIGHLIGHT } else { "" },
                if sel { '>' } else { ' ' },
                clip(base_name(&cia.path), 27),
                format_size(cia.size)
            );
        }

        t += "\x1b[K\n";
        if let Some(inst) = &self.inst {
            let done = *inst.shared.done.lock().unwrap();
            t += &format!(" Installing {}\x1b[K\n", clip(base_name(&inst.path), 27));
            t += &format!(
                "   {} / {}  {}%\x1b[K\n",
                format_size(done),
                format_size(inst.total),
                if inst.total != 0 { done * 100 / inst.total } else { 0 }
            );
        } else {
            t += "\x1b[K\n\x1b[K\n";
        }
        let color = if !show_msg {
            ""
        } else if self.cia_msg.starts_with("Installed") {
            "\x1b[32m"
        } else {
            "\x1b[31m"
        };
        t += &format!(" {}{}\x1b[0m\x1b[K\n", color, if show_msg { clip(&self.cia_msg, 38) } else { "" });
        if busy && !self.installing() {
            t += &format!("\x1b[30;1H{HIGHLIGHT} A: install (waits for transfers)\x1b[0m\x1b[K");
        } else {
            t += "\x1b[30;1H A: install + delete   Up/Down: select\x1b[K";
        }
        sys::select(Screen::Bottom);
        sys::print(&t);
        sys::select(Screen::Top);
    }

    pub(super) fn draw_top(&mut self, top: &Top) {
        let now = sys::now_ms();
        let (bat, low, active) = (top.bat, top.low, top.active);

        // every line on a fixed row, cleared to the end and kept under the 50
        // column width (a wrapped line used to leave old text behind)
        let mut t = String::new();
        t += &format!("\x1b[1;1H{TITLE} Cement Text\x1b[0m v{VERSION}\x1b[K");
        t += "\x1b[2;1H ------------------------------------------------\x1b[K";
        if self.net.ip != 0 {
            t += &format!("\x1b[4;1H IP   : {HIGHLIGHT}{}\x1b[0m\x1b[K", ip_text(self.net.ip));
        } else {
            t += "\x1b[4;1H IP   : -\x1b[K";
        }
        t += &format!("\x1b[5;1H Port : {HIGHLIGHT}{CTRL_PORT}\x1b[0m\x1b[K");
        t += &format!("\x1b[7;1H Status: {}\x1b[K", clip(&self.status, 40));
        t += &format!(
            "\x1b[8;1H Battery: {}{}\x1b[0m{}\x1b[K",
            if low { "\x1b[31m" } else { "" },
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
            t += "\x1b[13;1H\x1b[31m Battery low: new transfers and installs are\x1b[K\x1b[14;1H refused until the charger is plugged in\x1b[0m\x1b[K";
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
        sys::select(Screen::Top);
        sys::print(&t);
    }
}
