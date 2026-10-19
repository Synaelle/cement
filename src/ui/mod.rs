// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The two screens: status on the top one, the CIA list on the bottom one.
// What is shown and when is decided here, the same for both builds; text.rs
// (Cement Text, feature text-ui) prints it on the console, gfx/ draws it.

use crate::server::{MAX_SESSIONS, Mode};
use crate::{Battery, Server, battery_low, sys};

mod palette;

#[cfg(feature = "text-ui")]
mod text;
#[cfg(feature = "text-ui")]
pub use text::{BOTTOM_ALWAYS_ON, Ui};

#[cfg(not(feature = "text-ui"))]
mod gfx;
#[cfg(not(feature = "text-ui"))]
pub use gfx::{BOTTOM_ALWAYS_ON, Ui};

/// install and login messages: shown in green, the others (errors) in red
pub fn good_news(msg: &str) -> bool {
    msg.starts_with("Installed") || msg.starts_with("Login")
}

/// how long an install message stays on the bottom screen
const MSG_MS: u64 = 8000;

/// what the bottom screen shows besides the CIA list
pub struct Bottom {
    #[cfg_attr(feature = "text-ui", allow(dead_code))] // the console reprints everything
    pub dirty: bool,    // the list, the selection or the message changed
    pub show_msg: bool, // cia_msg is still recent
    pub busy: bool,     // transfers or file changes going on: an install waits
}

/// what the top screen shows besides the server fields
pub struct Top {
    #[cfg_attr(feature = "text-ui", allow(dead_code))] // in the status line there
    pub clients: usize,        // distinct PCs: the client opens a browse and a transfer connection
    pub active: Option<usize>, // session moving a file
    pub bat: Battery,
    pub low: bool,
}

impl Server {
    /// lights the bottom screen while it has something to show; Some when it needs drawing
    fn bottom(&mut self) -> Option<Bottom> {
        let now = sys::now_ms();
        let show_msg = !self.cia_msg.is_empty() && now.saturating_sub(self.cia_msg_time) < MSG_MS;
        let on = (BOTTOM_ALWAYS_ON || !self.cias.is_empty() || self.installing() || show_msg) && !self.idle_off;
        if on != self.bottom_on {
            self.bottom_on = on;
            sys::backlight(sys::SCREEN_BOTTOM, on);
        }
        let busy = self.fs_busy();
        if busy != self.last_busy {
            self.last_busy = busy;
            self.bottom_dirty = true;
        }
        if show_msg != self.last_msg {
            self.last_msg = show_msg; // the message timed out: clear it
            self.bottom_dirty = true;
        }
        if !on || (!self.bottom_dirty && !self.installing() && !show_msg) {
            return None;
        }
        let dirty = self.bottom_dirty;
        self.bottom_dirty = false;
        Some(Bottom { dirty, show_msg, busy })
    }

    /// every 500 ms: the transfer rate and the status line
    fn top(&mut self) -> Option<Top> {
        let now = sys::now_ms();
        if now.saturating_sub(self.last_time) < 500 {
            return None;
        }

        let mut clients = 0;
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
        let bat = self.battery();
        Some(Top { clients, active, bat, low: battery_low(bat) })
    }

    pub fn draw(&mut self) {
        if let Some(b) = self.bottom() {
            self.draw_bottom(&b);
        }
        if let Some(t) = self.top() {
            self.draw_top(&t);
        }
    }
}
