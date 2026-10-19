// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The 3DS side of the platform layer: libctru through ctru-sys.

// like the C server, the status of cleanup and best-effort calls is ignored
#![allow(unused_must_use)]

use std::ffi::c_void;
use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use ctru_sys as c;

use super::{AptEvent, Screen};

pub const KEY_A: u32 = c::KEY_A;
pub const KEY_START: u32 = c::KEY_START;
pub const KEY_SELECT: u32 = c::KEY_SELECT;
pub const KEY_UP: u32 = c::KEY_UP; // D-pad or circle pad
pub const KEY_DOWN: u32 = c::KEY_DOWN;

pub const MEDIA_NAND: u8 = c::MEDIATYPE_NAND;
pub const MEDIA_SD: u8 = c::MEDIATYPE_SD;

const SOC_ALIGN: usize = 0x1000;
const SOC_BUFSIZE: usize = 0x100000;

static SD: OnceLock<c::FS_Archive> = OnceLock::new(); // the SD card archive (no 64-bit atomics on the ARM11)
static MCU: AtomicBool = AtomicBool::new(false); // mcu::HWC gives a percentage
static PTMU: AtomicBool = AtomicBool::new(false); // ptm:u only 0-5 bars

fn ok(r: c::Result) -> bool {
    r >= 0
}

// ---------------------------------------------------------------------------
// startup and shutdown

static mut TOP: c::PrintConsole = unsafe { std::mem::zeroed() };
#[cfg(feature = "text-ui")]
static mut BOTTOM: c::PrintConsole = unsafe { std::mem::zeroed() };
static mut SOC_BUF: *mut u32 = std::ptr::null_mut();

/// false when the SD card can't be opened (nothing else is fatal)
pub fn init() -> bool {
    unsafe {
        c::acInit();
        c::ndmuInit();
        c::gfxInitDefault();
        c::amInit();
        PTMU.store(ok(c::ptmuInit()), Ordering::Relaxed);
        // may be denied to some .3dsx launchers: ptm:u bars then
        MCU.store(ok(c::mcuHwcInit()), Ordering::Relaxed);
        c::AM_InitializeExternalTitleDatabase(false); // as FBI does at startup
        #[cfg(feature = "text-ui")]
        {
            c::consoleInit(c::GFX_BOTTOM, &raw mut BOTTOM);
            c::consoleInit(c::GFX_TOP, &raw mut TOP);
        }

        let layout = std::alloc::Layout::from_size_align(SOC_BUFSIZE, SOC_ALIGN).unwrap();
        SOC_BUF = std::alloc::alloc(layout).cast();
        let mut sd: c::FS_Archive = 0;
        if SOC_BUF.is_null() || !ok(c::FSUSER_OpenArchive(&mut sd, c::ARCHIVE_SDMC, c::fsMakePath(c::PATH_EMPTY, c"".as_ptr().cast()))) {
            return false;
        }
        let _ = SD.set(sd);
    }
    true
}

/// shown when init failed, until START
pub fn fatal(msg: &str) {
    #[cfg(not(feature = "text-ui"))]
    unsafe {
        c::consoleInit(c::GFX_TOP, &raw mut TOP); // the drawn screens may not be up
    }
    print!("{msg}\nPress START to exit.\n");
    let _ = std::io::stdout().flush();
    unsafe {
        while c::aptMainLoop() {
            c::hidScanInput();
            if c::hidKeysDown() & KEY_START != 0 {
                break;
            }
            c::gspWaitForEvent(c::GSPGPU_EVENT_VBlank0, true);
        }
    }
}

pub fn exit() {
    unsafe {
        c::FSUSER_CloseArchive(sd());
        c::ndmuExit();
        if MCU.load(Ordering::Relaxed) {
            c::mcuHwcExit();
        }
        if PTMU.load(Ordering::Relaxed) {
            c::ptmuExit();
        }
        c::amExit();
        c::acExit();
        c::gfxExit();
    }
}

// ---------------------------------------------------------------------------
// time, input, screens

pub fn now_ms() -> u64 {
    unsafe { c::osGetTime() }
}

pub fn sleep_ms(ms: u64) {
    unsafe { c::svcSleepThread(ms as i64 * 1_000_000) }
}

pub fn keys_down() -> u32 {
    unsafe {
        c::hidScanInput();
        c::hidKeysDown()
    }
}

#[cfg(feature = "text-ui")]
pub fn select(screen: Screen) {
    unsafe {
        c::consoleSelect(match screen {
            Screen::Top => &raw mut TOP,
            Screen::Bottom => &raw mut BOTTOM,
        });
    }
}

/// console text (escape codes included)
#[cfg(feature = "text-ui")]
pub fn print(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// copies the rectangle x0..x1, y0..y1 of a screen image (0xRRGGBB, row by
/// row) into the framebuffer being drawn and shows it at the next VBlank
#[cfg(not(feature = "text-ui"))]
pub fn present(screen: Screen, px: &[u32], x0: usize, y0: usize, x1: usize, y1: usize) {
    static mut LAST: [u64; 2] = [0; 2];
    let (gs, w, i) = match screen {
        Screen::Top => (c::GFX_TOP, 400, 0),
        Screen::Bottom => (c::GFX_BOTTOM, 320, 1),
    };
    unsafe {
        // after a swap, the other buffer stays on screen until the next VBlank
        if now_ms().saturating_sub(LAST[i]) < 20 {
            c::gspWaitForEvent(c::GSPGPU_EVENT_VBlank0, true);
        }
        // BGR8, stored in columns from the bottom of the screen up
        let fb = c::gfxGetFramebuffer(gs, c::GFX_LEFT, std::ptr::null_mut(), std::ptr::null_mut());
        for x in x0..x1 {
            let col = fb.add(x * 240 * 3);
            for y in y0..y1 {
                let p = px[y * w + x];
                let o = col.add((239 - y) * 3);
                *o = p as u8;
                *o.add(1) = (p >> 8) as u8;
                *o.add(2) = (p >> 16) as u8;
            }
        }
        c::gfxFlushBuffers();
        c::gfxScreenSwapBuffers(gs, false);
        LAST[i] = now_ms();
    }
}

pub fn backlight(screens: u32, on: bool) {
    unsafe {
        if !ok(c::gspLcdInit()) {
            return;
        }
        if on {
            c::GSPLCD_PowerOnBacklight(screens);
        } else {
            c::GSPLCD_PowerOffBacklight(screens);
        }
        c::gspLcdExit();
    }
}

/// New 3DS 804 MHz + L2 cache; no effect on an Old 3DS
pub fn set_fast(on: bool) {
    unsafe { c::osSetSpeedupEnable(on) }
}

pub fn set_sleep_allowed(allowed: bool) {
    unsafe { c::aptSetSleepAllowed(allowed) }
}

// ---------------------------------------------------------------------------
// APT: the main loop and the sleep/suspend hooks

static mut HOOK: c::aptHookCookie = unsafe { std::mem::zeroed() };
static mut HANDLER: Option<(fn(AptEvent, *mut c_void), *mut c_void)> = None;

unsafe extern "C" fn on_apt(kind: c::APT_HookType, _: *mut c_void) {
    let event = match kind {
        c::APTHOOK_ONSUSPEND => AptEvent::Suspend,
        c::APTHOOK_ONRESTORE => AptEvent::Restore,
        c::APTHOOK_ONSLEEP => AptEvent::Sleep,
        c::APTHOOK_ONWAKEUP => AptEvent::Wakeup,
        _ => return,
    };
    if let Some((f, param)) = unsafe { HANDLER } {
        f(event, param);
    }
}

/// `f` runs inside apt_main_loop, on the main thread
pub fn apt_hook(f: fn(AptEvent, *mut c_void), param: *mut c_void) {
    unsafe {
        HANDLER = Some((f, param));
        c::aptHook(&raw mut HOOK, Some(on_apt), std::ptr::null_mut());
    }
}

pub fn apt_unhook() {
    unsafe {
        c::aptUnhook(&raw mut HOOK);
        HANDLER = None;
    }
}

pub fn apt_main_loop() -> bool {
    unsafe { c::aptMainLoop() }
}

// ---------------------------------------------------------------------------
// network services

pub fn wifi_connected() -> bool {
    let mut wifi = 0u32;
    unsafe { ok(c::ACU_GetWifiStatus(&mut wifi)) && wifi != 0 }
}

pub fn soc_init() -> bool {
    unsafe { ok(c::socInit(SOC_BUF, SOC_BUFSIZE as u32)) }
}

pub fn soc_exit() {
    unsafe {
        c::socExit();
    }
}

/// the console's IP as s_addr (network byte order in memory)
pub fn host_ip() -> u32 {
    unsafe { libc::gethostid() as u32 }
}

/// keeps StreetPass/SpotPass from using the Wi-Fi radio while we serve
pub fn ndm_enter() -> bool {
    unsafe {
        if !ok(c::NDMU_EnterExclusiveState(c::NDM_EXCLUSIVE_STATE_INFRASTRUCTURE)) {
            return false;
        }
        if ok(c::NDMU_LockState()) {
            return true;
        }
        c::NDMU_LeaveExclusiveState();
        false
    }
}

pub fn ndm_leave() {
    unsafe {
        c::NDMU_UnlockState();
        c::NDMU_LeaveExclusiveState();
    }
}

// ---------------------------------------------------------------------------
// battery

pub fn battery_percent() -> Option<u8> {
    let mut v = 0u8;
    (MCU.load(Ordering::Relaxed) && unsafe { ok(c::MCUHWC_GetBatteryLevel(&mut v)) }).then_some(v.min(100))
}

pub fn battery_bars() -> Option<u8> {
    let mut v = 0u8;
    (PTMU.load(Ordering::Relaxed) && unsafe { ok(c::PTMU_GetBatteryLevel(&mut v)) }).then_some(v)
}

pub fn charging() -> bool {
    let mut v = 0u8;
    PTMU.load(Ordering::Relaxed) && unsafe { ok(c::PTMU_GetBatteryChargeState(&mut v)) } && v != 0
}

pub fn plugged() -> bool {
    let mut v = false;
    PTMU.load(Ordering::Relaxed) && unsafe { ok(c::PTMU_GetAdapterState(&mut v)) } && v
}

// ---------------------------------------------------------------------------
// SD card (FSUSER on the SD archive: much faster than going through newlib)

const PATH_UNITS: usize = crate::util::PATH_SIZE;

/// UTF-16, NUL-terminated, for an FS_Path
struct FsPath(Vec<u16>);

impl FsPath {
    fn new(path: &str) -> Option<FsPath> {
        let mut v: Vec<u16> = path.encode_utf16().collect();
        if v.len() >= PATH_UNITS - 1 {
            return None;
        }
        v.push(0);
        Some(FsPath(v))
    }

    fn raw(&self) -> c::FS_Path {
        c::FS_Path { type_: c::PATH_UTF16, size: (self.0.len() * 2) as u32, data: self.0.as_ptr().cast() }
    }
}

fn sd() -> c::FS_Archive {
    SD.get().copied().unwrap_or(0)
}

pub struct File(c::Handle);

impl File {
    fn open(path: &str, flags: u32) -> Option<File> {
        let p = FsPath::new(path)?;
        let mut h: c::Handle = 0;
        unsafe { ok(c::FSUSER_OpenFile(&mut h, sd(), p.raw(), flags, 0)) }.then_some(File(h))
    }

    pub fn open_read(path: &str) -> Option<File> {
        File::open(path, c::FS_OPEN_READ as u32)
    }

    /// created if missing, never truncated
    pub fn open_write(path: &str) -> Option<File> {
        File::open(path, (c::FS_OPEN_WRITE | c::FS_OPEN_CREATE) as u32)
    }

    pub fn size(&self) -> Option<u64> {
        let mut size = 0u64;
        unsafe { ok(c::FSFILE_GetSize(self.0, &mut size)) }.then_some(size)
    }

    pub fn set_size(&self, size: u64) -> bool {
        unsafe { ok(c::FSFILE_SetSize(self.0, size)) }
    }

    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> Option<usize> {
        let mut n = 0u32;
        unsafe { ok(c::FSFILE_Read(self.0, &mut n, off, buf.as_mut_ptr().cast(), buf.len() as u32)) }.then_some(n as usize)
    }

    pub fn write_at(&self, off: u64, buf: &[u8]) -> Option<usize> {
        let mut n = 0u32;
        unsafe { ok(c::FSFILE_Write(self.0, &mut n, off, buf.as_ptr().cast(), buf.len() as u32, 0)) }.then_some(n as usize)
    }

    pub fn flush(&self) {
        unsafe {
            c::FSFILE_Flush(self.0);
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe {
            c::FSFILE_Close(self.0);
        }
    }
}

pub fn is_dir(path: &str) -> bool {
    let Some(p) = FsPath::new(path) else { return false };
    let mut h: c::Handle = 0;
    unsafe {
        if !ok(c::FSUSER_OpenDirectory(&mut h, sd(), p.raw())) {
            return false;
        }
        c::FSDIR_Close(h);
    }
    true
}

pub fn file_size(path: &str) -> Option<u64> {
    File::open_read(path)?.size()
}

/// milliseconds since 2000-01-01 on the console's clock (local time); as libctru's archive_getmtime
pub fn mtime(path: &str) -> Option<u64> {
    let p = FsPath::new(path)?;
    let raw = p.raw();
    let mut ms = 0u64;
    unsafe {
        ok(c::FSUSER_ControlArchive(
            sd(),
            c::ARCHIVE_ACTION_GET_TIMESTAMP,
            raw.data.cast_mut(),
            raw.size,
            (&raw mut ms).cast(),
            8,
        ))
    }
    .then_some(ms)
}

/// free and total bytes (total 0 when only the free space is known)
pub fn sd_space() -> Option<(u64, u64)> {
    let mut r = c::FS_ArchiveResource::default();
    unsafe {
        if ok(c::FSUSER_GetArchiveResource(&mut r, c::SYSTEM_MEDIATYPE_SD)) && r.clusterSize != 0 {
            return Some((r.freeClusters as u64 * r.clusterSize as u64, r.totalClusters as u64 * r.clusterSize as u64));
        }
        let mut free = 0u64;
        ok(c::FSUSER_GetFreeBytes(&mut free, sd())).then_some((free, 0))
    }
}

/// calls f(name, is_dir, size) for each entry; false if the folder can't be opened
pub fn list_dir(path: &str, f: &mut dyn FnMut(&str, bool, u64)) -> bool {
    let Some(p) = FsPath::new(path) else { return false };
    let mut h: c::Handle = 0;
    unsafe {
        if !ok(c::FSUSER_OpenDirectory(&mut h, sd(), p.raw())) {
            return false;
        }
    }
    let mut ents = vec![c::FS_DirectoryEntry::default(); 32];
    loop {
        let mut count = 0u32;
        if !unsafe { ok(c::FSDIR_Read(h, &mut count, 32, ents.as_mut_ptr())) } || count == 0 {
            break;
        }
        for e in &ents[..count as usize] {
            let len = e.name.iter().position(|&u| u == 0).unwrap_or(e.name.len());
            let Ok(name) = String::from_utf16(&e.name[..len]) else { continue };
            if name.is_empty() {
                continue;
            }
            let dir = e.attributes & c::FS_ATTRIBUTE_DIRECTORY != 0;
            f(&name, dir, if dir { 0 } else { e.fileSize });
        }
    }
    unsafe {
        c::FSDIR_Close(h);
    }
    true
}

pub fn delete_file(path: &str) -> bool {
    FsPath::new(path).is_some_and(|p| unsafe { ok(c::FSUSER_DeleteFile(sd(), p.raw())) })
}

pub fn delete_dir_recursive(path: &str) -> bool {
    FsPath::new(path).is_some_and(|p| unsafe { ok(c::FSUSER_DeleteDirectoryRecursively(sd(), p.raw())) })
}

pub fn create_dir(path: &str) -> bool {
    FsPath::new(path).is_some_and(|p| unsafe { ok(c::FSUSER_CreateDirectory(sd(), p.raw(), 0)) })
}

/// a file or a folder
pub fn rename(from: &str, to: &str) -> bool {
    let (Some(a), Some(b)) = (FsPath::new(from), FsPath::new(to)) else { return false };
    unsafe { ok(c::FSUSER_RenameFile(sd(), a.raw(), sd(), b.raw())) || ok(c::FSUSER_RenameDirectory(sd(), a.raw(), sd(), b.raw())) }
}

// ---------------------------------------------------------------------------
// threads: a given core needs libctru's threadCreate (std can't ask for one);
// any core is a normal std thread (pthread-3ds)

pub enum Joiner {
    Ctr(c::Thread),
    Std(std::thread::JoinHandle<()>),
}

unsafe extern "C" fn thread_entry(arg: *mut c_void) {
    let f: Box<Box<dyn FnOnce() + Send>> = unsafe { Box::from_raw(arg.cast()) };
    f();
}

/// core: -2 = the app's core (std thread), 2 = the New 3DS's extra core
pub fn spawn(core: i32, prio: i32, f: Box<dyn FnOnce() + Send>) -> Option<Joiner> {
    if core < 0 {
        return std::thread::Builder::new().stack_size(0x10000).spawn(f).ok().map(Joiner::Std);
    }
    let arg = Box::into_raw(Box::new(f)).cast::<c_void>();
    let t = unsafe { c::threadCreate(Some(thread_entry), arg, 0x10000, prio, core, false) };
    if t.is_null() {
        drop(unsafe { Box::from_raw(arg.cast::<Box<dyn FnOnce() + Send>>()) });
        return None;
    }
    Some(Joiner::Ctr(t))
}

impl Joiner {
    pub fn join(self) {
        match self {
            Joiner::Ctr(t) => unsafe {
                c::threadJoin(t, u64::MAX);
                c::threadFree(t);
            },
            Joiner::Std(h) => {
                let _ = h.join();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// title installs (AM), as FBI does them

pub struct AmInstall(c::Handle);

pub fn am_delete_title(media: u8, title: u64) {
    unsafe {
        c::AM_DeleteTitle(media, title);
    }
}

pub fn am_delete_ticket(title: u64) {
    unsafe {
        c::AM_DeleteTicket(title);
    }
}

/// the installed version of a title, None when it isn't installed
pub fn am_title_version(media: u8, title: u64) -> Option<u16> {
    let mut id = title;
    let mut info = c::AM_TitleInfo::default();
    unsafe { ok(c::AM_GetTitleInfo(media, 1, &mut id, &mut info)) }.then_some(info.version)
}

pub fn am_query_external_db() {
    unsafe {
        c::AM_QueryAvailableExternalTitleDatabase(std::ptr::null_mut());
    }
}

pub fn am_start(media: u8) -> Result<AmInstall, i32> {
    let mut h: c::Handle = 0;
    let r = unsafe { c::AM_StartCiaInstall(media, &mut h) };
    if ok(r) { Ok(AmInstall(h)) } else { Err(r) }
}

impl AmInstall {
    pub fn write_at(&self, off: u64, buf: &[u8]) -> Result<(), i32> {
        let mut n = 0u32;
        let r = unsafe { c::FSFILE_Write(self.0, &mut n, off, buf.as_ptr().cast(), buf.len() as u32, 0) };
        if ok(r) { Ok(()) } else { Err(r) }
    }

    pub fn finish(self) -> Result<(), i32> {
        let r = unsafe { c::AM_FinishCiaInstall(self.0) };
        if ok(r) { Ok(()) } else { Err(r) }
    }

    pub fn cancel(self) {
        unsafe {
            c::AM_CancelCIAInstall(self.0);
        }
    }
}

pub fn am_install_firm(title: u64) -> Result<(), i32> {
    let r = unsafe { c::AM_InstallFirm(title) };
    if ok(r) { Ok(()) } else { Err(r) }
}

pub fn is_new3ds() -> Option<bool> {
    let mut n3ds = false;
    unsafe { ok(c::APT_CheckNew3DS(&mut n3ds)) }.then_some(n3ds)
}

/// our own title ID and media, None when running as a .3dsx
pub fn self_title() -> Option<(u64, u8)> {
    let mut id = 0u64;
    let mut media = c::MEDIATYPE_NAND;
    unsafe {
        if c::envIsHomebrew() {
            return None;
        }
        ok(c::APT_GetAppletInfo(
            c::envGetAptAppId() as c::NS_APPID,
            &mut id,
            &mut media,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ))
        .then_some((id, media))
    }
}
