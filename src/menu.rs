// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The bottom screen's tabs, switched with L and R or by touching them:
// - Recent: the CIA files sent since Cement started, or found at startup
// - Browse: the SD card's folders, to open them and install a CIA from them
// - Settings: the colour (or sync with Baguette), the login, the FTP server, the
//   screens turning off, the console turning off when idle
// The state and the buttons are handled here; ui/gfx draws them and says what
// each touched spot is (Ui::hit).

use crate::settings::{self, AUTO_OFF};
use crate::util::{base_name, is_cia};
use crate::{Server, sys};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Recent,
    Browse,
    Settings,
}

pub const TABS: [Tab; 3] = [Tab::Recent, Tab::Browse, Tab::Settings];

impl Tab {
    pub fn name(self) -> &'static str {
        match self {
            Tab::Recent => "Recent",
            Tab::Browse => "Browse",
            Tab::Settings => "Settings",
        }
    }
}

/// Baguette's accent choices, offered in Settings
pub const SWATCHES: [u32; 9] = [0xb8283c, 0x951d2e, 0xd0503a, 0xc27a1e, 0x3f9a5a, 0x1f8f8a, 0x2f6fd0, 0x7a4cc4, 0xb8388f];

/// the Settings rows, top to bottom
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Row {
    Colour,
    Sync,
    Login,
    Ftp,
    ScreenIdle,
    AutoOff,
}

pub const ROWS: [Row; 6] = [Row::Colour, Row::Sync, Row::Login, Row::Ftp, Row::ScreenIdle, Row::AutoOff];

/// what a touch landed on (the drawing records where each one is)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tap {
    Tab(Tab),
    Item(usize), // a row of the current tab's list
    Swatch(usize),
}

pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// the Browse tab: one folder of the SD card
pub struct Browser {
    pub dir: String,
    pub entries: Vec<Entry>,
    pub sel: usize,
    pub loaded: bool, // listed since the last change (reloaded when it goes stale)
    pub failed: bool, // the folder can't be opened
}

/// listing a folder with this many entries is already slow on the SD card
const MAX_ENTRIES: usize = 1000;

pub struct Menu {
    pub tab: Tab,
    pub browse: Browser,
    pub row: usize, // Settings
}

impl Menu {
    pub fn new() -> Menu {
        Menu {
            tab: Tab::Recent,
            browse: Browser { dir: "/".into(), entries: Vec::new(), sel: 0, loaded: false, failed: false },
            row: 0,
        }
    }
}

fn parent(dir: &str) -> String {
    match dir.trim_end_matches('/').rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => dir[..i].to_string(),
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else { format!("{dir}/{name}") }
}

impl Server {
    /// L, R, the D-pad, A, B and touches on the bottom screen
    pub fn menu_input(&mut self, down: u32) {
        let mut tap = sys::touch().filter(|_| down & sys::KEY_TOUCH != 0).and_then(|(x, y)| self.ui.hit(x, y));
        let tab = self.menu.tab;
        let i = TABS.iter().position(|&t| t == tab).unwrap_or(0);
        let mut switch = None;
        if down & sys::KEY_L != 0 {
            switch = Some(TABS[(i + TABS.len() - 1) % TABS.len()]);
        } else if down & sys::KEY_R != 0 {
            switch = Some(TABS[(i + 1) % TABS.len()]);
        } else if let Some(Tap::Tab(t)) = tap {
            switch = Some(t);
            tap = None;
        }
        if let Some(t) = switch {
            if t != tab {
                self.menu.tab = t;
                self.menu.browse.loaded = false; // listed again: files come and go over FTP
                self.bottom_dirty = true;
            }
            return;
        }
        match tab {
            Tab::Recent => self.recent_input(down, tap),
            Tab::Browse => self.browse_input(down, tap),
            Tab::Settings => self.settings_input(down, tap),
        }
    }

    fn recent_input(&mut self, down: u32, tap: Option<Tap>) {
        match tap {
            // a touch selects a row, a second one on it installs
            Some(Tap::Item(i)) if i < self.cias.len() && self.inst.is_none() => {
                if i == self.cia_sel {
                    self.start_install();
                } else {
                    self.cia_sel = i;
                }
                self.bottom_dirty = true;
            }
            _ => self.cia_input(down),
        }
    }

    /// lists the Browse folder again when it changed or was never listed
    pub fn menu_refresh(&mut self) {
        if self.menu.tab != Tab::Browse || self.menu.browse.loaded {
            return;
        }
        let b = &mut self.menu.browse;
        let mut entries = Vec::new();
        b.failed = !sys::list_dir(&b.dir, &mut |name, dir, size| {
            if entries.len() < MAX_ENTRIES {
                entries.push(Entry { name: name.to_string(), dir, size });
            }
        });
        // folders first, then by name, as file managers do
        entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        b.entries = entries;
        b.sel = b.sel.min(b.entries.len().saturating_sub(1));
        b.loaded = true;
        self.bottom_dirty = true;
    }

    fn open_dir(&mut self, dir: String, select: Option<String>) {
        let b = &mut self.menu.browse;
        b.dir = dir;
        b.sel = 0;
        b.loaded = false;
        self.menu_refresh();
        if let Some(name) = select {
            let b = &mut self.menu.browse;
            b.sel = b.entries.iter().position(|e| e.name == name).unwrap_or(0);
        }
    }

    fn browse_activate(&mut self) {
        let b = &self.menu.browse;
        let Some(e) = b.entries.get(b.sel) else { return };
        let path = join(&b.dir, &e.name);
        if e.dir {
            self.open_dir(path, None);
        } else if is_cia(&path) {
            let reply = self.site_install(&path);
            let msg = match &reply[..3] {
                "200" if reply.contains("queued") => Some("The install starts when the transfers end".to_string()),
                "200" => None,
                _ => Some(reply[4..].trim_end().to_string()),
            };
            if let Some(msg) = msg {
                self.cia_message(msg);
            }
        } else {
            self.cia_message(format!("{} isn't a CIA file", base_name(&path)));
        }
    }

    fn browse_input(&mut self, down: u32, tap: Option<Tap>) {
        let n = self.menu.browse.entries.len();
        if let Some(Tap::Item(i)) = tap {
            if i < n {
                if i == self.menu.browse.sel {
                    self.browse_activate();
                } else {
                    self.menu.browse.sel = i;
                }
                self.bottom_dirty = true;
            }
            return;
        }
        let b = &mut self.menu.browse;
        if down & sys::KEY_UP != 0 && n != 0 {
            b.sel = (b.sel + n - 1) % n;
        } else if down & sys::KEY_DOWN != 0 && n != 0 {
            b.sel = (b.sel + 1) % n;
        } else if down & sys::KEY_LEFT != 0 && n != 0 {
            b.sel = b.sel.saturating_sub(8);
        } else if down & sys::KEY_RIGHT != 0 && n != 0 {
            b.sel = (b.sel + 8).min(n - 1);
        } else if down & sys::KEY_B != 0 {
            if b.dir != "/" {
                let (up, name) = (parent(&b.dir), base_name(&b.dir).to_string());
                self.open_dir(up, Some(name));
            }
        } else if down & sys::KEY_A != 0 {
            self.browse_activate();
        } else {
            return;
        }
        self.bottom_dirty = true;
    }

    fn save_prefs(&mut self) {
        settings::save_prefs(&self.prefs);
        self.bottom_dirty = true;
        self.last_time = 0; // the top screen may show it
    }

    fn set_accent(&mut self, accent: u32) {
        if accent != self.accent {
            self.accent = accent;
            settings::save_accent(accent);
        }
        self.bottom_dirty = true;
        self.last_time = 0;
    }

    /// the colour row: `step` swatches along (manual colour only)
    fn step_colour(&mut self, step: isize) {
        if self.prefs.color_sync {
            self.cia_message("The colour follows Baguette: turn the sync off to pick one".into());
            return;
        }
        let n = SWATCHES.len() as isize;
        let at = SWATCHES.iter().position(|&c| c == self.accent).map_or(if step > 0 { -1 } else { 0 }, |i| i as isize);
        self.set_accent(SWATCHES[(at + step).rem_euclid(n) as usize]);
    }

    fn step_auto_off(&mut self, step: isize) {
        let n = AUTO_OFF.len() as isize;
        let at = AUTO_OFF.iter().position(|&m| m == self.prefs.auto_off).unwrap_or(0) as isize;
        self.prefs.auto_off = AUTO_OFF[(at + step).rem_euclid(n) as usize];
        self.save_prefs();
    }

    /// A (or a touch) on a Settings row
    fn settings_activate(&mut self, row: Row) {
        match row {
            Row::Colour => self.step_colour(1),
            Row::Sync => {
                self.prefs.color_sync = !self.prefs.color_sync;
                self.save_prefs();
            }
            Row::Login => self.login_keyboard(),
            Row::Ftp => {
                self.prefs.ftp = !self.prefs.ftp;
                if !self.prefs.ftp {
                    self.net_down(); // every client goes, nothing listens
                    self.status = "FTP server off".into();
                } else {
                    self.status = "Starting...".into();
                    self.wake_net();
                }
                self.save_prefs();
            }
            Row::ScreenIdle => {
                self.prefs.screen_idle = !self.prefs.screen_idle;
                self.save_prefs();
            }
            Row::AutoOff => self.step_auto_off(1),
        }
    }

    fn settings_input(&mut self, down: u32, tap: Option<Tap>) {
        match tap {
            Some(Tap::Item(i)) if i < ROWS.len() => {
                self.menu.row = i;
                self.settings_activate(ROWS[i]);
                self.bottom_dirty = true;
                return;
            }
            Some(Tap::Swatch(k)) if k < SWATCHES.len() => {
                self.menu.row = 0;
                if self.prefs.color_sync {
                    self.step_colour(0); // says why it can't change
                } else {
                    self.set_accent(SWATCHES[k]);
                }
                return;
            }
            _ => {}
        }
        let n = ROWS.len();
        let row = ROWS[self.menu.row];
        if down & sys::KEY_UP != 0 {
            self.menu.row = (self.menu.row + n - 1) % n;
        } else if down & sys::KEY_DOWN != 0 {
            self.menu.row = (self.menu.row + 1) % n;
        } else if down & (sys::KEY_LEFT | sys::KEY_RIGHT) != 0 {
            let step = if down & sys::KEY_LEFT != 0 { -1 } else { 1 };
            match row {
                Row::Colour => self.step_colour(step),
                Row::AutoOff => self.step_auto_off(step),
                _ => return,
            }
        } else if down & sys::KEY_A != 0 {
            self.settings_activate(row);
        } else {
            return;
        }
        self.bottom_dirty = true;
    }
}
