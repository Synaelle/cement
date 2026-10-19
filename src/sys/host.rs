// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The PC test build of the platform layer, behaving like the old C test shim:
// - the SD card is a folder (env CEMENT_ROOT, default ".")
// - creating the file named by CEMENT_SLEEPFLAG closes the lid until it is deleted
// - a CIA "install" is written to the file named by CEMENT_AMOUT
// - keys come from the file named by CEMENT_KEYS ("A", "UP", "DOWN", "SELECT")
// - CEMENT_BATTERY="<percent>[,charging|,plugged]" (default 100), CEMENT_NO_MCU=1
//   hides the percentage, CEMENT_SDFREE fakes the free space
// - events the tests check are logged to stderr as "[shim] ..."

use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use super::{AptEvent, Screen};

pub const KEY_A: u32 = 1 << 0;
pub const KEY_SELECT: u32 = 1 << 2;
pub const KEY_START: u32 = 1 << 3;
pub const KEY_UP: u32 = 1 << 6;
pub const KEY_DOWN: u32 = 1 << 7;

pub const MEDIA_NAND: u8 = 0;
pub const MEDIA_SD: u8 = 1;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

// ---------------------------------------------------------------------------
// startup and shutdown

pub fn init() -> bool {
    super::net::startup();
    true
}

pub fn fatal(msg: &str) {
    eprintln!("{msg}");
}

pub fn exit() {}

// ---------------------------------------------------------------------------
// time, input, screens

fn start() -> &'static Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now)
}

pub fn now_ms() -> u64 {
    1000 + start().elapsed().as_millis() as u64
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

pub fn keys_down() -> u32 {
    let file = env("CEMENT_KEYS").unwrap_or_else(|| "keys.txt".into());
    let Ok(text) = fs::read_to_string(&file) else { return 0 };
    let _ = fs::remove_file(&file);
    let key = text.trim();
    if key.starts_with("SELECT") {
        KEY_SELECT
    } else if key.starts_with("UP") {
        KEY_UP
    } else if key.starts_with("DOWN") {
        KEY_DOWN
    } else if key.starts_with('A') {
        KEY_A
    } else {
        0
    }
}

pub fn select(_: Screen) {}

/// the screens aren't shown on a PC (keeps the test log readable)
pub fn print(_: &str) {}

pub fn backlight(screens: u32, on: bool) {
    eprintln!("[shim] backlight {}={}", if on { "on" } else { "off" }, screens);
}

pub fn set_fast(on: bool) {
    eprintln!("[shim] fast={}", on as i32);
}

pub fn set_sleep_allowed(allowed: bool) {
    eprintln!("[shim] sleep allowed={}", allowed as i32);
}

// ---------------------------------------------------------------------------
// APT: closing the lid is simulated with the sleep flag file

static mut HANDLER: Option<(fn(AptEvent, *mut c_void), *mut c_void)> = None;

pub fn apt_hook(f: fn(AptEvent, *mut c_void), param: *mut c_void) {
    unsafe { HANDLER = Some((f, param)) }
}

pub fn apt_unhook() {
    unsafe { HANDLER = None }
}

pub fn apt_main_loop() -> bool {
    let flag = env("CEMENT_SLEEPFLAG").unwrap_or_else(|| "sleep.flag".into());
    if let Some((f, param)) = unsafe { HANDLER } {
        if fs::metadata(&flag).is_ok() {
            eprintln!("[shim] sleeping");
            f(AptEvent::Sleep, param);
            while fs::metadata(&flag).is_ok() {
                sleep_ms(50);
            }
            eprintln!("[shim] woke up");
            f(AptEvent::Wakeup, param);
        }
    }
    true
}

// ---------------------------------------------------------------------------
// network services

pub fn wifi_connected() -> bool {
    true
}

pub fn soc_init() -> bool {
    true
}

pub fn soc_exit() {
    eprintln!("[shim] socExit");
}

pub fn host_ip() -> u32 {
    u32::from_ne_bytes([127, 0, 0, 1])
}

pub fn ndm_enter() -> bool {
    true
}

pub fn ndm_leave() {}

// ---------------------------------------------------------------------------
// battery

fn battery_env() -> String {
    env("CEMENT_BATTERY").unwrap_or_default()
}

fn battery_level() -> i32 {
    let v = battery_env();
    if v.is_empty() { 100 } else { crate::util::parse_ull(v.as_bytes()) as i32 }
}

pub fn battery_percent() -> Option<u8> {
    env("CEMENT_NO_MCU").is_none().then(|| battery_level().clamp(0, 255) as u8)
}

pub fn battery_bars() -> Option<u8> {
    let p = battery_level();
    Some(match p {
        ..=0 => 0,
        1..=10 => 1,
        11..=30 => 2,
        31..=60 => 3,
        61..=90 => 4,
        _ => 5,
    })
}

pub fn charging() -> bool {
    battery_env().contains("charging")
}

pub fn plugged() -> bool {
    charging() || battery_env().contains("plugged")
}

// ---------------------------------------------------------------------------
// SD card: a folder

fn real(path: &str) -> PathBuf {
    let root = env("CEMENT_ROOT").unwrap_or_else(|| ".".into());
    PathBuf::from(format!("{root}{path}"))
}

fn too_long(path: &str) -> bool {
    path.encode_utf16().count() >= crate::util::PATH_SIZE - 1
}

pub struct File(fs::File);

impl File {
    pub fn open_read(path: &str) -> Option<File> {
        let p = real(path);
        if too_long(path) || p.is_dir() {
            return None;
        }
        fs::File::open(p).ok().map(File)
    }

    /// created if missing, never truncated
    pub fn open_write(path: &str) -> Option<File> {
        let p = real(path);
        if too_long(path) || p.is_dir() {
            return None;
        }
        fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(p).ok().map(File)
    }

    pub fn size(&self) -> Option<u64> {
        self.0.metadata().ok().map(|m| m.len())
    }

    pub fn set_size(&self, size: u64) -> bool {
        self.0.set_len(size).is_ok()
    }

    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> Option<usize> {
        let mut done = 0;
        while done < buf.len() {
            match read_at(&self.0, off + done as u64, &mut buf[done..]) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(_) => return None,
            }
        }
        Some(done)
    }

    pub fn write_at(&self, off: u64, buf: &[u8]) -> Option<usize> {
        let mut done = 0;
        while done < buf.len() {
            match write_at(&self.0, off + done as u64, &buf[done..]) {
                Ok(0) | Err(_) => return None,
                Ok(n) => done += n,
            }
        }
        Some(done)
    }

    pub fn flush(&self) {
        let _ = self.0.sync_all();
    }
}

#[cfg(unix)]
fn read_at(f: &fs::File, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, off)
}
#[cfg(unix)]
fn write_at(f: &fs::File, off: u64, buf: &[u8]) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, buf, off)
}
#[cfg(windows)]
fn read_at(f: &fs::File, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, off)
}
#[cfg(windows)]
fn write_at(f: &fs::File, off: u64, buf: &[u8]) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, buf, off)
}

pub fn is_dir(path: &str) -> bool {
    !too_long(path) && real(path).is_dir()
}

pub fn file_size(path: &str) -> Option<u64> {
    File::open_read(path)?.size()
}

/// like the 3DS: milliseconds since 2000-01-01 on a clock that keeps local time
/// (the file's local modification time, read as if it were UTC)
pub fn mtime(path: &str) -> Option<u64> {
    let modified = fs::metadata(real(path)).ok()?.modified().ok()?;
    let utc = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let local = utc + local_offset(utc);
    Some(((local - 946_684_800).max(0) as u64) * 1000)
}

#[cfg(unix)]
fn local_offset(utc: i64) -> i64 {
    unsafe {
        let t = utc as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm.tm_gmtoff as i64
    }
}

#[cfg(windows)]
fn local_offset(utc: i64) -> i64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    let ft = (utc as u64 + 11_644_473_600) * 10_000_000;
    let a = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let mut b = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    unsafe { FileTimeToLocalFileTime(&a, &mut b) };
    let local = ((b.dwHighDateTime as u64) << 32) | b.dwLowDateTime as u64;
    (local as i64 - ft as i64) / 10_000_000
}

/// the disk holding CEMENT_ROOT, in 32 KB clusters like an SD card
pub fn sd_space() -> Option<(u64, u64)> {
    let (free, total) = disk_space(&real(""))?;
    let free = env("CEMENT_SDFREE").map_or(free, |v| crate::util::parse_ull(v.as_bytes()));
    Some((free / 32768 * 32768, total / 32768 * 32768))
}

#[cfg(unix)]
fn disk_space(root: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(root.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut v: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(c.as_ptr(), &mut v) == 0)
            .then(|| (v.f_bavail as u64 * v.f_frsize as u64, v.f_blocks as u64 * v.f_frsize as u64))
    }
}

#[cfg(windows)]
fn disk_space(root: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    let (mut avail, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, std::ptr::null_mut()) != 0 }
        .then_some((avail, total))
}

pub fn list_dir(path: &str, f: &mut dyn FnMut(&str, bool, u64)) -> bool {
    if too_long(path) {
        return false;
    }
    let Ok(dir) = fs::read_dir(real(path)) else { return false };
    for e in dir.flatten() {
        let Ok(meta) = fs::metadata(e.path()) else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        f(&name, meta.is_dir(), if meta.is_dir() { 0 } else { meta.len() });
    }
    true
}

pub fn delete_file(path: &str) -> bool {
    let p = real(path);
    !too_long(path) && p.is_file() && fs::remove_file(p).is_ok()
}

pub fn delete_dir_recursive(path: &str) -> bool {
    !too_long(path) && fs::remove_dir_all(real(path)).is_ok()
}

pub fn create_dir(path: &str) -> bool {
    !too_long(path) && fs::create_dir(real(path)).is_ok()
}

/// like the 3DS: fails when the target exists
pub fn rename(from: &str, to: &str) -> bool {
    let (a, b) = (real(from), real(to));
    !too_long(from) && !too_long(to) && a.exists() && !b.exists() && fs::rename(a, b).is_ok()
}

// ---------------------------------------------------------------------------
// threads

pub struct Joiner(std::thread::JoinHandle<()>);

pub fn spawn(_core: i32, _prio: i32, f: Box<dyn FnOnce() + Send>) -> Option<Joiner> {
    std::thread::Builder::new().spawn(f).ok().map(Joiner)
}

impl Joiner {
    pub fn join(self) {
        let _ = self.0.join();
    }
}

// ---------------------------------------------------------------------------
// title installs: written to the file named by CEMENT_AMOUT

pub struct AmInstall(fs::File);

pub fn am_delete_title(media: u8, title: u64) {
    eprintln!("[shim] AM_DeleteTitle media={media} id={title:016x}");
}

pub fn am_delete_ticket(_: u64) {}

pub fn am_query_external_db() {}

pub fn am_start(media: u8) -> Result<AmInstall, i32> {
    let out = env("CEMENT_AMOUT").unwrap_or_else(|| "am-installed.bin".into());
    let f = fs::File::create(out).map_err(|_| -1)?;
    eprintln!("[shim] AM_StartCiaInstall media={media}");
    Ok(AmInstall(f))
}

impl AmInstall {
    pub fn write_at(&self, off: u64, buf: &[u8]) -> Result<(), i32> {
        let mut done = 0;
        while done < buf.len() {
            match write_at(&self.0, off + done as u64, &buf[done..]) {
                Ok(0) | Err(_) => return Err(-1),
                Ok(n) => done += n,
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<(), i32> {
        eprintln!("[shim] AM_FinishCiaInstall");
        Ok(())
    }

    pub fn cancel(self) {
        eprintln!("[shim] AM_CancelCIAInstall");
    }
}

pub fn am_install_firm(_: u64) -> Result<(), i32> {
    Ok(())
}

/// the test build plays an Old 3DS
pub fn is_new3ds() -> Option<bool> {
    Some(false)
}

/// Cement itself, installed on the SD card
pub fn self_title() -> Option<(u64, u8)> {
    Some((0x0004_0000_0F7A_5100, MEDIA_SD))
}
