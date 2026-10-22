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
    pub done: Mutex<u64>,     // bytes installed (no 64-bit atomics on the ARM11)
    pub preparing: AtomicBool, // removing the installed copy first (can take a while)
    finished: AtomicBool,
    rc: AtomicI32,
    err: Mutex<&'static str>,
}

/// the last finished install, for SITE STATUS (until the next one starts)
pub struct InstallResult {
    pub n: u32, // counts finished installs from 1
    pub ok: bool,
    pub code: u32, // the failing result code, 0 on success
    pub path: String,
    pub msg: String, // what the bottom screen said
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

    /// the SD card root, /cia, /cias and /fbi, so the list survives a restart
    /// (a listing each: cheap)
    pub fn scan_cias(&mut self) {
        for dir in ["/", "/cia", "/cias", "/fbi"] {
            let mut found = Vec::new();
            sys::list_dir(dir, &mut |name, is_dir, size| {
                if !is_dir && is_cia(name) {
                    found.push((format!("{}/{name}", dir.trim_end_matches('/')), size));
                }
            });
            found.sort();
            for (path, size) in found {
                self.cia_add(&path, size);
            }
        }
    }

    /// SITE INSTALL: what pressing A on that file does; the FTP reply
    pub fn site_install(&mut self, path: &str) -> &'static str {
        if self.inst.is_some() || self.inst_queue.is_some() {
            return "450 Busy: another install is running\r\n";
        }
        let size = if is_cia(path) { sys::file_size(path) } else { None };
        let Some(size) = size else { return "550 No such CIA\r\n" };
        if battery_low(self.battery()) {
            return "451 Battery too low: plug in the charger\r\n";
        }
        if !self.cias.iter().any(|c| c.path == path) {
            self.cia_add(path, size);
        }
        self.cia_sel = self.cias.iter().position(|c| c.path == path).unwrap_or(0);
        self.last_wake = sys::now_ms(); // wakes the screens
        self.bottom_dirty = true;
        if self.fs_busy() {
            self.inst_queue = Some(path.to_string()); // starts when the transfers end
            return "200 Install queued\r\n";
        }
        self.start_install();
        if self.inst.is_some() { "200 Install started\r\n" } else { "451 Install could not start\r\n" }
    }

    /// starts a queued SITE INSTALL once the SD card is free
    fn poll_queue(&mut self) {
        if self.inst.is_some() || self.fs_busy() {
            return;
        }
        let Some(path) = self.inst_queue.take() else { return };
        if let Some(i) = self.cias.iter().position(|c| c.path == path) {
            self.cia_sel = i;
            self.last_wake = sys::now_ms();
            self.start_install();
        }
    }

    pub fn installing(&self) -> bool {
        self.inst.is_some()
    }

    pub fn cia_message(&mut self, msg: String) {
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
            preparing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            rc: AtomicI32::new(0),
            err: Mutex::new(""),
        });
        self.cia_msg.clear();
        sys::set_sleep_allowed(false); // never sleep in the middle of an install
        let (p, s) = (path.clone(), shared.clone());
        match sys::spawn(-2, 0x30, Box::new(move || install(&p, total, &s))) {
            Some(t) => {
                self.inst = Some(Install { path, total, shared, thread: Some(t) });
                self.install_result = None;
            }
            None => {
                sys::set_sleep_allowed(true);
                self.cia_message("Could not start the install".into());
            }
        }
        self.bottom_dirty = true;
    }

    pub fn poll_install(&mut self) {
        self.poll_queue();
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
        self.menu.browse.loaded = false; // the installed file is gone, or the list is the same
        if rc >= 0 {
            self.cia_remove(&inst.path);
            self.cia_message(format!("Installed {}", util::clip(base_name(&inst.path), 30)));
        } else {
            let err = *inst.shared.err.lock().unwrap();
            self.cia_message(format!("{} ({:08X})", util::clip(err, 30), rc as u32));
        }
        self.install_count += 1;
        self.install_result = Some(InstallResult {
            n: self.install_count,
            ok: rc >= 0,
            code: if rc >= 0 { 0 } else { rc as u32 },
            path: inst.path,
            msg: self.cia_msg.clone(),
        });
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

/// the title ID and title version in the CIA's TMD (FBI cia_get_title_id +
/// tmd_get_title_id; the version is the big-endian u16 at 0x9C of the TMD header)
fn cia_title(cia: &[u8]) -> Option<(u64, Option<u16>)> {
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
    let header = offset + SIG_SIZES[cia[offset + 3] as usize];
    if header + 0x4C + 8 > cia.len() {
        return None;
    }
    let id = u64::from_be_bytes(cia[header + 0x4C..header + 0x54].try_into().unwrap());
    let version = cia.get(header + 0x9C..header + 0x9E).map(|v| u16::from_be_bytes([v[0], v[1]]));
    Some((id, version))
}

/// what the top screen shows about the selected CIA
pub struct CiaInfo {
    pub title: String,     // the SMDH short title, or the file name
    pub publisher: String, // empty when the CIA has no SMDH
    pub id: u64,
    pub version: Option<u16>,
    pub installed: Option<u16>, // the version installed now, None when it isn't
    pub nand: bool,
    pub icon: Option<Vec<u32>>, // ICON x ICON pixels, 0xRRGGBB
}

/// the drawn icon size: the 48 px SMDH icon scaled down
pub const ICON: usize = 32;

/// the CIA header's size, cert chain, ticket, TMD and meta sizes, and the
/// content size; each section starts 64-byte aligned
fn cia_sections(h: &[u8]) -> Option<([usize; 5], u64)> {
    if h.len() < 0x20 {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(h[i * 4..i * 4 + 4].try_into().unwrap()) as usize;
    let content = u64::from_le_bytes(h[0x18..0x20].try_into().unwrap());
    Some(([word(0), word(2), word(3), word(4), word(5)], content))
}

/// UTF-16LE up to the first 0; None when the font can't draw it (Latin-1 only)
fn utf16(b: &[u8]) -> Option<String> {
    let units = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0);
    let s: String = char::decode_utf16(units).collect::<Result<_, _>>().ok()?;
    let s = s.trim().to_string();
    (!s.is_empty() && s.chars().all(|c| (c as u32) < 0x100 && !c.is_control())).then_some(s)
}

/// the 48 x 48 RGB565 SMDH icon (8 x 8 tiles, Z-order inside them), scaled
/// to ICON x ICON by averaging
fn icon(raw: &[u8]) -> Vec<u32> {
    const N: usize = 48;
    let mut src = vec![[0f32; 3]; N * N];
    for (i, px) in raw.chunks_exact(2).take(N * N).enumerate() {
        let v = u16::from_le_bytes([px[0], px[1]]) as u32;
        let (tile, k) = (i / 64, i % 64);
        let x = (tile % (N / 8)) * 8 + ((k & 1) | ((k >> 1) & 2) | ((k >> 2) & 4));
        let y = (tile / (N / 8)) * 8 + (((k >> 1) & 1) | ((k >> 2) & 2) | ((k >> 3) & 4));
        let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
        src[y * N + x] = [(r * 255 / 31) as f32, (g * 255 / 63) as f32, (b * 255 / 31) as f32];
    }
    // how much of source pixel i falls in destination pixel d, along one axis
    let scale = N as f32 / ICON as f32;
    let weight = |d: usize, i: usize| {
        let (a, b) = (d as f32 * scale, (d + 1) as f32 * scale);
        ((i + 1) as f32).min(b) - (i as f32).max(a)
    };
    let mut out = Vec::with_capacity(ICON * ICON);
    for dy in 0..ICON {
        for dx in 0..ICON {
            let mut sum = [0f32; 3];
            let (y0, x0) = ((dy as f32 * scale) as usize, (dx as f32 * scale) as usize);
            for y in y0..(y0 + 2).min(N) {
                for x in x0..(x0 + 2).min(N) {
                    let w = weight(dy, y).max(0.0) * weight(dx, x).max(0.0);
                    for c in 0..3 {
                        sum[c] += src[y * N + x][c] * w;
                    }
                }
            }
            let ch = |v: f32| ((v / (scale * scale)).round() as u32).min(255);
            out.push(ch(sum[0]) << 16 | ch(sum[1]) << 8 | ch(sum[2]));
        }
    }
    out
}

/// the title, publisher and icon (from the SMDH in the CIA's meta section)
/// and the title ID and version (from the TMD); None when it isn't a CIA
pub fn cia_info(path: &str) -> Option<CiaInfo> {
    let f = File::open_read(path)?;
    let mut head = vec![0u8; 0x4000];
    let n = f.read_at(0, &mut head)?;
    let (id, version) = cia_title(&head[..n])?;
    let name = base_name(path);
    let mut info = CiaInfo {
        title: name.strip_suffix(".cia").or(name.strip_suffix(".CIA")).unwrap_or(name).to_string(),
        publisher: String::new(),
        id,
        version,
        installed: None,
        nand: title_destination(id) == sys::MEDIA_NAND,
        icon: None,
    };
    info.installed = sys::am_title_version(title_destination(id), id);
    // the meta section: 0x400 bytes of dependencies and such, then the SMDH
    let (sizes, content) = cia_sections(&head[..n])?;
    let align = |v: u64| (v + 0x3F) & !0x3F;
    let meta = sizes[..4].iter().map(|&s| align(s as u64)).sum::<u64>() + align(content);
    if sizes[4] >= 0x400 + 0x36C0 {
        let mut smdh = vec![0u8; 0x36C0];
        if f.read_at(meta + 0x400, &mut smdh) == Some(smdh.len()) && &smdh[..4] == b"SMDH" {
            // English first, then any language the font can draw
            let titles = |lang: usize| {
                let t = &smdh[8 + lang * 0x200..][..0x200];
                utf16(&t[..0x80]).map(|short| (short, utf16(&t[0x180..0x200]).unwrap_or_default()))
            };
            if let Some((title, publisher)) = [1].into_iter().chain(0..16).find_map(titles) {
                (info.title, info.publisher) = (title, publisher);
            }
            info.icon = Some(icon(&smdh[0x24C0..0x24C0 + 48 * 48 * 2]));
        }
    }
    Some(info)
}

/// "v1.2.0" from a title version (6 bits minor, 4 bits micro)
pub fn version_text(v: u16) -> String {
    format!("v{}.{}.{}", v >> 10, (v >> 4) & 0x3F, v & 0xF)
}

/// The installed copy has to go first only for a reinstall or a downgrade (AM
/// refuses those over it, D8E08027). An update or a new title installs over it
/// directly: the delete is slow, so it is skipped whenever it can be.
fn must_delete(installed: Option<u16>, version: Option<u16>) -> bool {
    match (installed, version) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(old), Some(new)) => new <= old,
    }
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
                    let Some((id, version)) = cia_title(&buf[..n]) else {
                        fail(&mut r, 0, "Not a valid CIA file");
                        break;
                    };
                    title = id;
                    let dest = title_destination(id);
                    if sys::is_new3ds() == Some(false) && (id >> 28) & 0xF == 2 {
                        fail(&mut r, 0, "This title is for New 3DS only");
                        break;
                    }
                    // like FBI, but only when AM needs it; Cement can't delete itself, so
                    // the same or an older version of it can't go over it
                    let delete = must_delete(sys::am_title_version(dest, id), version);
                    if delete && sys::self_title() == Some((id, dest)) {
                        fail(&mut r, 0, "Use FBI to reinstall Cement");
                        break;
                    }
                    if delete {
                        shared.preparing.store(true, Ordering::Relaxed);
                        sys::am_delete_title(dest, id);
                        sys::am_delete_ticket(id);
                        if dest == sys::MEDIA_SD {
                            sys::am_query_external_db();
                        }
                        shared.preparing.store(false, Ordering::Relaxed);
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
    fn deletes() {
        assert!(!must_delete(None, Some(3))); // not installed
        assert!(!must_delete(Some(2), Some(3))); // update
        assert!(must_delete(Some(3), Some(3))); // reinstall
        assert!(must_delete(Some(4), Some(3))); // downgrade
    }

    #[test]
    fn destinations() {
        assert_eq!(title_destination(0x0004_0000_0012_3400), sys::MEDIA_SD);
        assert_eq!(title_destination(0x0004_0010_0002_0000), sys::MEDIA_NAND); // system
        assert_eq!(title_destination(0x0003_0000_0000_0000), sys::MEDIA_NAND); // DSiWare
    }
}
