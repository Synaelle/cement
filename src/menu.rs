// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The bottom screen's tabs, switched with L and R or by touching them:
// - Recent: the CIA files sent since Cement started, or found at startup
// - Browse: the SD card's folders, to open them and install a CIA from them;
//   A on an image shows it on the top screen until B (or A again)
// On both, X deletes the selected file (pressed twice: it can't be undone);
// Browse deletes folders and any file too, except the important 3DS files
// while their protection is on (protect.rs).
// - Settings: the login, the FTP server, the CIA details on the top screen, the
//   protection of the important 3DS files, the screens turning off, the console
//   turning off when idle (picking the colour is off until the redesign is done)
// The state and the buttons are handled here; ui/gfx draws them and says what
// each touched spot is (Ui::hit).

use crate::picture::is_image;
use crate::protect;
use crate::settings::{self, AUTO_OFF, LOOKS, Look};
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

/// the Settings rows, top to bottom
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Row {
    Look,
    Login,
    Ftp,
    CiaInfo,
    Protect,
    ScreenIdle,
    AutoOff,
}

pub const ROWS: [Row; 7] = [Row::Look, Row::Login, Row::Ftp, Row::CiaInfo, Row::Protect, Row::ScreenIdle, Row::AutoOff];

/// what a touch landed on (the drawing records where each one is)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tap {
    Tab(Tab),
    Item(usize), // a row of the current tab's list
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
    pub row: usize,                     // Settings
    pub view: bool,                     // Browse: the selected image is shown on the top screen
    pub setup: Option<usize>,           // first launch: the look highlighted (LOOKS), until A picks it
    pub held: bool,                     // the Up or Down being handled is a repeat of a held key
    hold: Option<(u32, u64, u64)>,      // Up or Down held: which, since when, the next repeat
    confirm: Option<(String, u64)>,     // what a first press asked about, and until when a second one confirms it
}

/// Up or Down held: the first repeat after HOLD_MS, then one every REPEAT_MS,
/// every FAST_MS once held for FAST_AFTER_MS
const HOLD_MS: u64 = 300;
const REPEAT_MS: u64 = 90;
const FAST_MS: u64 = 30;
const FAST_AFTER_MS: u64 = 1000;

/// the selection one row up or down: a press wraps around the list, a held
/// key stops at its ends (holding doesn't run past them)
pub fn step(sel: usize, n: usize, up: bool, held: bool) -> usize {
    match (up, held) {
        (true, false) => (sel + n - 1) % n,
        (false, false) => (sel + 1) % n,
        (true, true) => sel.saturating_sub(1),
        (false, true) => (sel + 1).min(n - 1),
    }
}

/// how long a second press has to come to confirm (deleting, turning the protection off):
/// as long as the question stays on screen, so a press while it shows always confirms
const CONFIRM_MS: u64 = crate::ui::MSG_MS;

impl Menu {
    pub fn new() -> Menu {
        Menu {
            tab: Tab::Recent,
            browse: Browser { dir: "/".into(), entries: Vec::new(), sel: 0, loaded: false, failed: false },
            row: 0,
            view: false,
            setup: None,
            held: false,
            hold: None,
            confirm: None,
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
    /// Up and Down again while held, faster the longer: how many repeats are
    /// due now (several when a loop round took longer than one)
    pub fn held_repeats(&mut self, down: u32) -> (u32, u32) {
        let now = sys::now_ms();
        let arrows = down & (sys::KEY_UP | sys::KEY_DOWN);
        if arrows != 0 {
            self.menu.hold = Some((arrows, now, now + HOLD_MS));
            return (0, 0);
        }
        let Some((key, since, mut next)) = self.menu.hold else { return (0, 0) };
        if sys::keys_held() & key == 0 {
            self.menu.hold = None;
            return (0, 0);
        }
        let mut n = 0;
        while now >= next && n < 8 {
            next += if next - since >= FAST_AFTER_MS { FAST_MS } else { REPEAT_MS };
            n += 1;
        }
        self.menu.hold = Some((key, since, next.max(now + 1)));
        (key, n)
    }

    /// the look drawn: the highlighted one while it is being picked
    pub fn look(&self) -> Look {
        self.menu.setup.map_or(self.prefs.look, |i| LOOKS[i])
    }

    /// first launch: Up and Down preview a look on both screens, A (or a touch on it) picks it
    fn setup_input(&mut self, down: u32, tap: Option<Tap>) {
        let Some(at) = self.menu.setup else { return };
        let n = LOOKS.len();
        let pick = match tap {
            Some(Tap::Item(i)) if i < n && i == at => true,
            Some(Tap::Item(i)) if i < n => {
                self.menu.setup = Some(i);
                false
            }
            _ if down & (sys::KEY_UP | sys::KEY_DOWN) != 0 => {
                self.menu.setup = Some(step(at, n, down & sys::KEY_UP != 0, self.menu.held));
                false
            }
            _ => down & sys::KEY_A != 0,
        };
        if pick {
            self.prefs.look = LOOKS[at];
            self.menu.setup = None;
            self.save_prefs();
        }
        self.bottom_dirty = true;
        self.last_time = 0; // the top screen previews it now
    }

    /// L, R, the D-pad, A, B and touches on the bottom screen
    pub fn menu_input(&mut self, down: u32) {
        let mut tap = sys::touch().filter(|_| down & sys::KEY_TOUCH != 0).and_then(|(x, y)| self.ui.hit(x, y));
        if self.menu.setup.is_some() {
            if down != 0 {
                self.setup_input(down, tap);
            }
            return;
        }
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
                self.menu.view = false;
                if self.menu.confirm.take().is_some() {
                    self.cia_msg.clear(); // a question about the other tab's selection
                }
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

    /// the image shown on the top screen: Browse's selection while viewing
    pub fn viewed_image(&self) -> Option<String> {
        let b = &self.menu.browse;
        if self.menu.tab != Tab::Browse || !self.menu.view {
            return None;
        }
        b.entries.get(b.sel).filter(|e| !e.dir && is_image(&e.name)).map(|e| join(&b.dir, &e.name))
    }

    /// the CIA file selected in Recent or Browse, and its size
    pub fn selected_cia(&self) -> Option<(String, u64)> {
        match self.menu.tab {
            Tab::Recent => self.cias.get(self.cia_sel).map(|c| (c.path.clone(), c.size)),
            Tab::Browse => {
                let b = &self.menu.browse;
                b.entries.get(b.sel).filter(|e| !e.dir && is_cia(&e.name)).map(|e| (join(&b.dir, &e.name), e.size))
            }
            Tab::Settings => None,
        }
    }

    /// true when `what` was asked about within CONFIRM_MS (the second press),
    /// else asks with `question`
    fn confirm(&mut self, what: &str, question: String) -> bool {
        let now = sys::now_ms();
        if self.menu.confirm.as_ref().is_some_and(|(w, until)| w == what && now < *until) {
            self.menu.confirm = None;
            return true;
        }
        self.cia_message(question);
        self.menu.confirm = Some((what.to_string(), now + CONFIRM_MS));
        false
    }

    /// X on a file or a folder: the first press asks, a second one within CONFIRM_MS deletes it
    fn ask_delete(&mut self, path: String, dir: bool) {
        let name = base_name(&path).to_string();
        self.bottom_dirty = true;
        if self.prefs.protect {
            if let Some(why) = protect::delete(&path) {
                self.cia_message(why);
                return;
            }
        }
        let inside = |p: &str| p == path || (dir && p.starts_with(&format!("{}/", path.trim_end_matches('/'))));
        if self.inst.as_ref().is_some_and(|i| inside(&i.path)) || self.inst_queue.as_deref().is_some_and(inside) {
            self.cia_message(format!("{name} is being installed"));
            return;
        }
        if dir && self.fs_busy() {
            self.cia_message("Busy: wait for the transfers to end".into());
            return;
        }
        let question = if dir { format!("Press X again to delete {name} and all in it") } else { format!("Press X again to delete {name}") };
        if !self.confirm(&path, question) {
            return;
        }
        let ok = if dir {
            // one call that can take a while: say so first
            self.cia_message(format!("Deleting {name}..."));
            self.draw();
            sys::delete_dir_recursive(&path)
        } else {
            sys::delete_file(&path)
        };
        if ok {
            self.cia_remove(&path); // the CIAs in it leave Recent
            self.menu.browse.loaded = false;
            self.cia_message(format!("Deleted {name}"));
        } else {
            self.cia_message(format!("{name} couldn't be deleted"));
        }
    }

    fn recent_input(&mut self, down: u32, tap: Option<Tap>) {
        if down & sys::KEY_X != 0 {
            if let Some(cia) = self.cias.get(self.cia_sel) {
                self.ask_delete(cia.path.clone(), false);
            }
            return;
        }
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
        self.menu.view = false;
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
        } else if is_image(&path) {
            self.menu.view = !self.menu.view;
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
        if down & (sys::KEY_UP | sys::KEY_DOWN) != 0 && n != 0 {
            b.sel = step(b.sel, n, down & sys::KEY_UP != 0, self.menu.held);
        } else if down & sys::KEY_LEFT != 0 && n != 0 {
            b.sel = b.sel.saturating_sub(8);
        } else if down & sys::KEY_RIGHT != 0 && n != 0 {
            b.sel = (b.sel + 8).min(n - 1);
        } else if down & sys::KEY_B != 0 && self.menu.view {
            self.menu.view = false;
        } else if down & sys::KEY_B != 0 {
            if b.dir != "/" {
                let (up, name) = (parent(&b.dir), base_name(&b.dir).to_string());
                self.open_dir(up, Some(name));
            }
        } else if down & sys::KEY_A != 0 {
            self.browse_activate();
        } else if down & sys::KEY_X != 0 {
            if let Some(e) = b.entries.get(b.sel) {
                let (path, dir) = (join(&b.dir, &e.name), e.dir);
                self.ask_delete(path, dir);
            }
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

    fn step_look(&mut self, step: isize) {
        let n = LOOKS.len() as isize;
        let at = LOOKS.iter().position(|&l| l == self.prefs.look).unwrap_or(0) as isize;
        self.prefs.look = LOOKS[(at + step).rem_euclid(n) as usize];
        self.save_prefs();
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
            Row::Look => self.step_look(1),
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
            Row::CiaInfo => {
                self.prefs.cia_info = !self.prefs.cia_info;
                self.save_prefs();
            }
            Row::Protect => {
                // off lets X delete what keeps the 3DS booting: asked first
                if self.prefs.protect && !self.confirm("protect", "Press A again to turn the protection off".into()) {
                    return;
                }
                self.prefs.protect = !self.prefs.protect;
                self.cia_message(if self.prefs.protect { "Protection on" } else { "Protection off: X deletes anything" }.into());
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
            _ => {}
        }
        let n = ROWS.len();
        let row = ROWS[self.menu.row];
        if down & (sys::KEY_UP | sys::KEY_DOWN) != 0 {
            self.menu.row = step(self.menu.row, n, down & sys::KEY_UP != 0, self.menu.held);
        } else if down & (sys::KEY_LEFT | sys::KEY_RIGHT) != 0 {
            let step = if down & sys::KEY_LEFT != 0 { -1 } else { 1 };
            match row {
                Row::AutoOff => self.step_auto_off(step),
                Row::Look => self.step_look(step),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps() {
        assert_eq!(step(0, 5, true, false), 4, "a press wraps to the end");
        assert_eq!(step(4, 5, false, false), 0, "and back to the top");
        assert_eq!(step(0, 5, true, true), 0, "held, it stops at the top");
        assert_eq!(step(4, 5, false, true), 4, "and at the bottom");
        assert_eq!(step(2, 5, false, true), 3);
    }
}
