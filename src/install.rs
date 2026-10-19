// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Uploaded CIA files (bottom screen list) and installing them, following FBI
// (source/fbi/action/installcias.c).
// FBI: Copyright (C) 2015-2019 Steveice10, MIT license (see NOTICE)

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use crate::disk::{Buf, CHUNK};
use crate::sys::{self, File};
use crate::util::{self, base_name, is_cia};
use crate::{Server, battery_low};

const MAX_CIAS: usize = 64;

pub struct Cia {
    pub path: String,
    pub size: u64,
}

/// what the install thread reports
pub struct Shared {
    pub done: Mutex<u64>, // bytes installed (no 64-bit atomics on the ARM11)
    finished: AtomicBool,
    rc: AtomicI32,
    err: Mutex<&'static str>,
}

pub struct Install {
    pub path: String,
    pub total: u64,
    pub shared: Arc<Shared>,
    thread: Option<sys::Joiner>,
}

impl Server {
    /// removes path, or everything under it when it is a folder
    pub fn cia_remove(&mut self, path: &str) {
        let before = self.cias.len();
        self.cias.retain(|c| {
            !(c.path == path
                || (c.path.starts_with(path) && (path == "/" || c.path.as_bytes().get(path.len()) == Some(&b'/'))))
        });
        if self.cias.len() != before {
            self.bottom_dirty = true;
        }
        if self.cia_sel >= self.cias.len() {
            self.cia_sel = self.cias.len().saturating_sub(1);
        }
    }

    pub fn cia_add(&mut self, path: &str, size: u64) {
        if !is_cia(path) {
            return;
        }
        self.cia_remove(path);
        if self.cias.len() == MAX_CIAS {
            return;
        }
        self.cias.push(Cia { path: path.to_string(), size });
        self.bottom_dirty = true;
    }

    pub fn installing(&self) -> bool {
        self.inst.is_some()
    }

    fn cia_message(&mut self, msg: String) {
        self.cia_msg = msg;
        self.cia_msg_time = sys::now_ms();
        self.bottom_dirty = true;
    }

    pub fn start_install(&mut self) {
        if self.inst.is_some() || self.cia_sel >= self.cias.len() {
            return;
        }
        let bat = self.battery();
        if battery_low(bat) {
            self.cia_message("Battery too low: plug in the charger".into());
            return;
        }
        if self.fs_busy() {
            self.cia_message("Busy: wait for the transfer to end".into());
            return;
        }
        let cia = &self.cias[self.cia_sel];
        let (path, total) = (cia.path.clone(), cia.size);
        let shared = Arc::new(Shared {
            done: Mutex::new(0),
            finished: AtomicBool::new(false),
            rc: AtomicI32::new(0),
            err: Mutex::new(""),
        });
        self.cia_msg.clear();
        sys::set_sleep_allowed(false); // never sleep in the middle of an install
        let (p, s) = (path.clone(), shared.clone());
        match sys::spawn(-2, 0x30, Box::new(move || install(&p, total, &s))) {
            Some(t) => self.inst = Some(Install { path, total, shared, thread: Some(t) }),
            None => {
                sys::set_sleep_allowed(true);
                self.cia_message("Could not start the install".into());
            }
        }
        self.bottom_dirty = true;
    }

    pub fn poll_install(&mut self) {
        let Some(inst) = &mut self.inst else { return };
        if !inst.shared.finished.load(Ordering::Acquire) {
            return;
        }
        if let Some(t) = inst.thread.take() {
            t.join();
        }
        let inst = self.inst.take().unwrap();
        sys::set_sleep_allowed(true);
        let rc = inst.shared.rc.load(Ordering::Relaxed);
        if rc >= 0 {
            self.cia_remove(&inst.path);
            self.cia_message(format!("Installed {}", util::clip(base_name(&inst.path), 30)));
        } else {
            let err = *inst.shared.err.lock().unwrap();
            self.cia_message(format!("{} ({:08X})", util::clip(err, 30), rc as u32));
        }
    }

    pub fn cia_input(&mut self, down: u32) {
        if self.inst.is_some() || self.cias.is_empty() {
            return;
        }
        let n = self.cias.len();
        if down & sys::KEY_UP != 0 {
            self.cia_sel = (self.cia_sel + n - 1) % n;
        } else if down & sys::KEY_DOWN != 0 {
            self.cia_sel = (self.cia_sel + 1) % n;
        } else if down & sys::KEY_A != 0 {
            self.start_install();
        } else {
            return;
        }
        self.bottom_dirty = true;
    }
}

/// FBI fs_get_title_destination: DSiWare, system and DLP titles go to NAND
fn title_destination(title: u64) -> u8 {
    let platform = (title >> 48) as u16;
    let category = (title >> 32) as u16;
    let variation = title as u8;
    let nand = platform == 0x0003
        || (platform == 0x0004 && ((category & 0x8011) != 0 || (category == 0x0000 && variation == 0x02)));
    if nand { sys::MEDIA_NAND } else { sys::MEDIA_SD }
}

/// FBI cia_get_title_id + tmd_get_title_id
fn cia_title_id(cia: &[u8]) -> Option<u64> {
    const SIG_SIZES: [usize; 6] = [0x240, 0x140, 0x80, 0x240, 0x140, 0x80];
    if cia.len() < 0x10 {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(cia[i * 4..i * 4 + 4].try_into().unwrap()) as usize;
    let align = |v: usize| (v + 0x3F) & !0x3F;
    let offset = align(word(0)) + align(word(2)) + align(word(3));
    if offset + 4 > cia.len() || cia[offset + 3] >= 6 {
        return None;
    }
    let pos = offset + SIG_SIZES[cia[offset + 3] as usize] + 0x4C;
    if pos + 8 > cia.len() {
        return None;
    }
    Some(u64::from_be_bytes(cia[pos..pos + 8].try_into().unwrap()))
}

/// the install thread
fn install(path: &str, total: u64, shared: &Shared) {
    let mut r: i32 = 0;
    let mut err = "";
    let mut fail = |r: &mut i32, rc: i32, msg: &'static str| {
        *r = if rc < 0 { rc } else if *r < 0 { *r } else { -1 };
        err = msg;
    };
    let mut am: Option<sys::AmInstall> = None;
    let mut title = 0u64;
    let src = File::open_read(path);

    match (Buf::new(), &src) {
        (None, _) => fail(&mut r, 0, "Out of memory"),
        (_, None) => fail(&mut r, 0, "Can't open the file"),
        (Some(mut buf), Some(src)) => {
            if total == 0 {
                fail(&mut r, 0, "Empty file");
            }
            let mut off = 0u64;
            while r >= 0 && off < total {
                let n = match src.read_at(off, &mut buf[..CHUNK]) {
                    Some(n) if n > 0 => n,
                    _ => {
                        fail(&mut r, 0, "SD card read error");
                        break;
                    }
                };
                if am.is_none() {
                    let Some(id) = cia_title_id(&buf[..n]) else {
                        fail(&mut r, 0, "Not a valid CIA file");
                        break;
                    };
                    title = id;
                    let dest = title_destination(id);
                    if sys::is_new3ds() == Some(false) && (id >> 28) & 0xF == 2 {
                        fail(&mut r, 0, "This title is for New 3DS only");
                        break;
                    }
                    // like FBI: replace an existing install, except when updating ourselves
                    if sys::self_title() != Some((id, dest)) {
                        sys::am_delete_title(dest, id);
                        sys::am_delete_ticket(id);
                        if dest == sys::MEDIA_SD {
                            sys::am_query_external_db();
                        }
                    }
                    match sys::am_start(dest) {
                        Ok(h) => am = Some(h),
                        Err(rc) => {
                            fail(&mut r, rc, "Install could not start");
                            break;
                        }
                    }
                }
                if let Err(rc) = am.as_ref().unwrap().write_at(off, &buf[..n]) {
                    fail(&mut r, rc, "Install write failed");
                    break;
                }
                off += n as u64;
                *shared.done.lock().unwrap() = off;
            }
        }
    }

    if let Some(am) = am {
        if r >= 0 {
            match am.finish() {
                Err(rc) => fail(&mut r, rc, "Install could not finish"),
                Ok(()) => {
                    if title & 0xFFF_FFFF == 0x2 {
                        if let Err(rc) = sys::am_install_firm(title) {
                            fail(&mut r, rc, "FIRM install failed");
                        }
                    }
                }
            }
        } else {
            am.cancel();
        }
    }
    drop(src);
    if r >= 0 {
        sys::delete_file(path);
    }
    *shared.err.lock().unwrap() = err;
    shared.rc.store(r, Ordering::Relaxed);
    shared.finished.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        assert_eq!(title_destination(0x0004_0000_0012_3400), sys::MEDIA_SD);
        assert_eq!(title_destination(0x0004_0010_0002_0000), sys::MEDIA_NAND); // system
        assert_eq!(title_destination(0x0003_0000_0000_0000), sys::MEDIA_NAND); // DSiWare
    }
}
