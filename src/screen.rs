// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The two screens: status on the top one, the CIA list on the bottom one.

use crate::server::{CTRL_PORT, MAX_SESSIONS, Mode, progress_of};
use crate::sys::{self, Screen};
use crate::util::{base_name, clip, format_size, ip_text};
use crate::{Server, VERSION, battery_low, battery_text};

impl Server {
    fn draw_bottom(&mut self) {
        let now = sys::now_ms();
        let show_msg = !self.cia_msg.is_empty() && now.saturating_sub(self.cia_msg_time) < 8000;
        let on = (!self.cias.is_empty() || self.installing() || show_msg) && !self.idle_off;
        if on != self.bottom_on {
            self.bottom_on = on;
            sys::backlight(sys::SCREEN_BOTTOM, on);
        }
        let busy = self.fs_busy();
        if busy != self.last_busy {
            self.last_busy = busy;
            self.bottom_dirty = true;
        }
        if !on || (!self.bottom_dirty && !self.installing() && !show_msg) {
            return;
        }
        self.bottom_dirty = false;

        let mut t = String::new();
        t += "\x1b[1;1H\x1b[36m CIA files ready to install\x1b[0m\x1b[K\n";
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
                if sel { "\x1b[33m" } else { "" },
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
            t += "\x1b[30;1H\x1b[33m A: install (waits for transfers)\x1b[0m\x1b[K";
        } else {
            t += "\x1b[30;1H A: install + delete   Up/Down: select\x1b[K";
        }
        sys::select(Screen::Bottom);
        sys::print(&t);
        sys::select(Screen::Top);
    }

    pub fn draw(&mut self) {
        let now = sys::now_ms();
        self.draw_bottom();
        if now.saturating_sub(self.last_time) < 500 {
            return;
        }

        let mut clients = 0; // distinct PCs: the client opens a browse and a transfer connection
        let mut bytes = 0u64;
        let mut active = None;
        for i in 0..MAX_SESSIONS {
            let s = &self.sess[i];
            if s.ctrl.is_none() {
                continue;
            }
            let seen = (0..i).any(|j| self.sess[j].ctrl.is_some() && self.sess[j].peer == s.peer);
            clients += !seen as usize;
            bytes += s.x.done;
            if !s.name.is_empty() && (s.mode == Mode::Stor || s.mode == Mode::Retr) {
                active = Some(i);
            }
        }
        let mut rate = 0;
        if active.is_some() && bytes >= self.last_bytes {
            rate = (bytes - self.last_bytes) * 1000 / (now - self.last_time).max(1);
        }
        self.rate = (self.rate + rate * 3) / 4;
        self.last_bytes = bytes;
        self.last_time = now;

        if self.net.up && self.net.listen.is_some() {
            self.status = if clients != 0 { format!("{clients} PC(s) connected") } else { "Ready".into() };
        }

        // every line on a fixed row, cleared to the end and kept under the 50
        // column width (a wrapped line used to leave old text behind)
        let mut t = String::new();
        t += &format!("\x1b[1;1H\x1b[31m Cement\x1b[0m v{VERSION}\x1b[K");
        t += "\x1b[2;1H ------------------------------------------------\x1b[K";
        if self.net.ip != 0 {
            t += &format!("\x1b[4;1H IP   : \x1b[33m{}\x1b[0m\x1b[K", ip_text(self.net.ip));
        } else {
            t += "\x1b[4;1H IP   : -\x1b[K";
        }
        t += &format!("\x1b[5;1H Port : \x1b[33m{CTRL_PORT}\x1b[0m\x1b[K");
        t += &format!("\x1b[7;1H Status: {}\x1b[K", clip(&self.status, 40));
        let bat = self.battery();
        let low = battery_low(bat);
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
