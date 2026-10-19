// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Cement - minimal, fast FTP server for the 3DS (SD card only)
//
// Design:
// - one non-blocking poll() loop on the main thread handles every socket
// - one disk thread performs all SD reads/writes, so network receive and
//   SD writes overlap (NBUF buffers of CHUNK bytes per session)
// - closing the lid lets the system sleep: the network is torn down in the
//   APT sleep hook and brought back up after wake-up
// - uploaded .cia files are listed on the bottom screen; A installs one
//   (same steps as FBI) and deletes the file afterwards
//
// Built for the 3DS (target armv6k-nintendo-3ds) it uses libctru; built for a
// PC it is a test server that plays the 3DS (see sys/host.rs).

mod disk;
mod install;
mod server;
mod settings;
mod sys;
mod ui;
mod untar;
mod util;

use std::ffi::c_void;

use disk::Disk;
use install::{Cia, Install, InstallResult};
use server::{MAX_SESSIONS, Net, Session};
use sys::AptEvent;

#[cfg(target_os = "horizon")]
use ctru as _; // links what std needs on the 3DS (pthread-3ds, shim-3ds)

/// set by the build: <VERSION file>.<commit count>-<commit>[-dev]
pub const VERSION: &str = match option_env!("CEMENT_VERSION") {
    Some(v) => v,
    None => "dev",
};

/// Compatibility level with the PC apps, reported in SITE STATUS. Bump it (and
/// REQUIRED_PROTOCOL in Chomp's src/ftp.h) only when the PC side needs this new
/// 3DS build; Chomp warns about an outdated 3DS app only then.
pub const PROTOCOL: u32 = 3;

const fn env_ms(v: Option<&str>, default: u64) -> u64 {
    let Some(v) = v else { return default };
    let b = v.as_bytes();
    let mut n = 0u64;
    let mut i = 0;
    while i < b.len() {
        n = n * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    n
}

// power: the screens turn off after an idle minute, and the New 3DS fast mode
// (804 MHz, L2 cache) only runs while something is being done. The tests build
// with shorter times (CEMENT_SCREEN_IDLE_MS, CEMENT_FAST_LINGER_MS).
const SCREEN_IDLE_MS: u64 = env_ms(option_env!("CEMENT_SCREEN_IDLE_MS"), 60 * 1000); // no button, transfer or install
const FAST_LINGER_MS: u64 = env_ms(option_env!("CEMENT_FAST_LINGER_MS"), 5000); // keeps fast mode between the files of a job

// below this, on battery power, new transfers and installs are refused: the
// console dying in the middle of an SD card write can corrupt the file system
const BATTERY_LOW: i32 = 5; // percent

#[derive(Clone, Copy)]
pub struct Battery {
    pub percent: i32, // -1 when unknown (mcu::HWC denied: only the 0-5 bars)
    pub bars: i32,
    pub charging: bool,
    pub plugged: bool,
}

pub fn battery_low(b: Battery) -> bool {
    if b.plugged || b.charging {
        return false;
    }
    if b.percent >= 0 { b.percent <= BATTERY_LOW } else { b.bars >= 0 && b.bars <= 1 }
}

pub fn battery_text(b: Battery) -> String {
    if b.percent >= 0 {
        format!("{}%", b.percent)
    } else if b.bars >= 0 {
        format!("{}/5", b.bars)
    } else {
        "?".into()
    }
}

pub struct Server {
    pub sess: Vec<Session>,
    pub net: Net,
    pub disk: Disk,
    pub status: String,
    next_net_check: u64,
    pub fs_busy_until: u64, // a delete/rename/mkdir happened recently (blocks CIA installs)
    pub last_activity: u64, // last transfer end or file system change (SITE STATUS idle_ms)

    // screens
    backlight: bool, // top screen wanted on (SELECT)
    pub bottom_on: bool,
    pub bottom_dirty: bool,
    pub last_msg: bool, // the install message was shown at the last draw
    pub last_busy: bool,
    pub last_bytes: u64,
    pub last_time: u64,
    pub rate: u64,

    // power
    pub idle_off: bool, // the screens are off because of the idle timer (not SELECT)
    last_wake: u64,     // last button press, transfer or install
    fast: bool,
    fast_until: u64,
    battery_cache: Option<(u64, Battery)>,

    // CIA installs
    pub cias: Vec<Cia>,
    pub cia_sel: usize,
    pub inst: Option<Install>,
    pub cia_msg: String,
    pub cia_msg_time: u64,
    pub inst_queue: Option<String>, // SITE INSTALL waiting for the transfers to end
    pub install_count: u32,
    pub install_result: Option<InstallResult>,

    pub accent: u32,                     // drawn UI accent, set by Chomp (SITE COLOR)
    pub login: Option<(String, String)>, // FTP user and password; None: anonymous
    pub ui: ui::Ui,
}

impl Server {
    fn new() -> Server {
        Server {
            sess: (0..MAX_SESSIONS).map(|_| Session::new()).collect(),
            net: Net::new(),
            disk: Disk::start(),
            status: "Starting...".into(),
            next_net_check: 0,
            fs_busy_until: 0,
            last_activity: sys::now_ms(), // idle_ms counts from startup
            backlight: true,
            bottom_on: false,
            bottom_dirty: true,
            last_msg: false,
            last_busy: false,
            last_bytes: 0,
            last_time: 0,
            rate: 0,
            idle_off: false,
            last_wake: 0,
            fast: false,
            fast_until: 0,
            battery_cache: None,
            cias: Vec::new(),
            cia_sel: 0,
            inst: None,
            cia_msg: String::new(),
            cia_msg_time: 0,
            inst_queue: None,
            install_count: 0,
            install_result: None,
            accent: 0,
            login: None,
            ui: ui::Ui::new(),
        }
    }

    /// cached: asked for on every transfer and status
    pub fn battery(&mut self) -> Battery {
        let now = sys::now_ms();
        if let Some((at, b)) = self.battery_cache {
            if now - at < 2000 {
                return b;
            }
        }
        let b = Battery {
            percent: sys::battery_percent().map_or(-1, |v| v as i32),
            bars: sys::battery_bars().map_or(-1, |v| v as i32),
            charging: sys::charging(),
            plugged: sys::plugged(),
        };
        self.battery_cache = Some((now, b));
        b
    }

    fn apply_backlight(&mut self) {
        // the bottom screen is lit while it has something to show (ui::Server::bottom)
        sys::backlight(sys::SCREEN_BOTTOM, self.bottom_on);
        self.idle_off = false;
        self.last_wake = sys::now_ms();
        sys::backlight(sys::SCREEN_TOP, self.backlight);
    }

    fn working(&self) -> bool {
        self.installing() || self.sess.iter().any(|s| s.working())
    }

    /// true when a button press only woke a screen up (so it does nothing else)
    fn manage_power(&mut self, down: u32) -> bool {
        let now = sys::now_ms();
        let busy = self.working();
        if busy || down != 0 {
            self.last_wake = now;
        }
        if busy {
            self.fast_until = now + FAST_LINGER_MS;
        }

        let fast = now < self.fast_until;
        if fast != self.fast {
            self.fast = fast;
            sys::set_fast(fast); // no effect on an Old 3DS
        }

        // the bottom screen follows idle_off in ui::Server::bottom
        let idle = now.saturating_sub(self.last_wake) >= SCREEN_IDLE_MS;
        if self.idle_off && !idle {
            self.idle_off = false;
            self.bottom_dirty = true;
            if self.backlight {
                sys::backlight(sys::SCREEN_TOP, true);
            }
            // a press that lit a screen does nothing else; with both staying dark it acts as
            // usual, and SELECT still works when only the bottom screen came back
            let bottom = !self.cias.is_empty() || ui::BOTTOM_ALWAYS_ON;
            return down != 0 && (self.backlight || (bottom && down & sys::KEY_SELECT == 0));
        }
        if !self.idle_off && idle {
            self.idle_off = true;
            if self.backlight {
                sys::backlight(sys::SCREEN_TOP, false);
            }
        }
        false
    }

    /// one main loop iteration; false to quit
    fn step(&mut self) -> bool {
        let mut down = sys::keys_down();
        if self.manage_power(down) {
            down = 0; // the press only turned a screen back on
        }
        self.poll_install();
        if down & sys::KEY_START != 0 {
            if !self.installing() {
                return false;
            }
            self.cia_msg = "Wait for the install to finish".into();
            self.cia_msg_time = sys::now_ms();
            self.bottom_dirty = true;
        }
        self.cia_input(down);
        if down & sys::KEY_Y != 0 {
            self.login_keyboard();
        }
        if down & sys::KEY_SELECT != 0 {
            self.backlight = !self.backlight;
            sys::backlight(sys::SCREEN_TOP, self.backlight);
        }

        let now = sys::now_ms();
        if now >= self.next_net_check {
            self.next_net_check = now + 1000;
            if !self.net.up {
                self.net_up();
            } else if !sys::wifi_connected() {
                self.net_down();
                self.status = "Wi-Fi lost, waiting...".into();
            }
        }

        if self.net.up {
            self.serve();
        } else {
            sys::sleep_ms(50);
        }
        self.draw();
        true
    }
}

impl Server {
    /// Y: the FTP login typed on the console (an empty user name clears it)
    fn login_keyboard(&mut self) {
        let current = self.login.as_ref().map(|l| l.0.clone()).unwrap_or_default();
        let msg = match sys::keyboard("FTP user name (empty: anyone can connect)", &current, false, 32) {
            None => None,
            Some(user) if user.trim().is_empty() => {
                self.set_login(None);
                Some("Login cleared: anyone can connect".to_string())
            }
            Some(user) if !settings::valid_user(user.trim()) => Some("A user name can't have spaces".to_string()),
            Some(user) => {
                let user = user.trim().to_string();
                sys::keyboard(&format!("Password for {user}"), "", true, 64).map(|pass| {
                    self.set_login(Some((user.clone(), pass)));
                    format!("Login set for {user}")
                })
            }
        };
        if let Some(msg) = msg {
            self.cia_msg = msg;
            self.cia_msg_time = sys::now_ms();
        }
        // the keyboard drew over both screens
        self.ui.invalidate();
        self.bottom_dirty = true;
        self.last_time = 0;
    }
}

/// runs inside the APT main loop on the main thread (param: the Server)
fn on_apt(event: AptEvent, param: *mut c_void) {
    let s = unsafe { &mut *param.cast::<Server>() };
    match event {
        AptEvent::Sleep => {
            // lid closed: Wi-Fi goes away, drop everything cleanly
            s.net_down();
            sys::backlight(sys::SCREEN_BOTH, true);
        }
        AptEvent::Suspend => sys::backlight(sys::SCREEN_BOTH, true), // so the HOME menu is visible
        AptEvent::Wakeup | AptEvent::Restore => {
            s.ui.invalidate(); // the HOME menu drew over the screens
            s.apply_backlight();
            s.next_net_check = 0;
        }
    }
}

fn main() {
    if !sys::init() {
        sys::fatal("Init failed (memory or SD card).");
        sys::exit();
        return;
    }

    // only ever used through this pointer: the APT hook needs it too
    let mut s = Box::new(Server::new());
    let saved = settings::load();
    (s.accent, s.login) = (saved.accent, saved.login);
    s.scan_cias();
    let server: *mut Server = Box::into_raw(s);
    sys::apt_hook(on_apt, server.cast());
    unsafe { (*server).apply_backlight() };

    while sys::apt_main_loop() {
        if !unsafe { (*server).step() } {
            break;
        }
    }

    let mut server = unsafe { Box::from_raw(server) };
    server.net_down();
    server.disk.stop();
    sys::apt_unhook();
    sys::backlight(sys::SCREEN_BOTH, true);
    sys::exit();
}
