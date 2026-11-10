// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: AGPL-3.0-or-later
// Updates: once the network is up after Cement starts, a thread asks GitHub
// for the latest release. A newer one shows in Settings' Updates row (which
// also turns the check off), where A streams its Cement.cia to the SD card and
// installs it with the usual install code (install.rs), which deletes it after.
// The installed title is Cement itself at a newer title version, so the 3DS
// takes it over the running copy; it runs once Cement is restarted.

use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::http::{self, Fail};
use crate::sys::{self, File};
use crate::{Server, battery_low};

const API: &str = "https://api.github.com/repos/Synaelle/cement/releases/latest";
const DOWNLOAD: &str = "https://github.com/Synaelle/cement/releases/download/{tag}/Cement.cia";

/// where the download goes (not one of the folders Recent lists)
pub const FILE: &str = "/3ds/cement/update.cia";
/// Cement's title ID (meta/Cement.rsf): the download has to be Cement
const TITLE: u64 = 0x0004_0000_0F7A_5100;

const MAX_JSON: u64 = 512 * 1024; // the release's JSON, notes included
const MAX_CIA: u64 = 64 << 20;
const STALL_MS: u64 = 30_000; // no byte for that long: the connection is cut
const STACK: usize = 0x40000; // the TLS handshake needs more than the usual 64 KB

/// (major, minor, micro) of "v1.2.0", "1.2.0" or "1.2.0-abc1234-dev" (a dev build counts as its base version)
pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim();
    let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
    let base = v.split(['-', '+']).next()?;
    let num =
        |p: &str| (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).then(|| p.parse::<u32>().ok()).flatten();
    let mut n = base.split('.').map(num);
    let v = (n.next()??, n.next()??, n.next()??);
    n.next().is_none().then_some(v)
}

/// whether the release `tag` is newer than the version `own`
pub fn newer(tag: &str, own: &str) -> bool {
    matches!((parse_version(tag), parse_version(own)), (Some(t), Some(o)) if t > o)
}

/// a tag that can go into the download URL as it is
fn valid_tag(t: &str) -> bool {
    !t.is_empty() && t.len() <= 64 && t.chars().all(|c| c.is_ascii_alphanumeric() || ".-_+".contains(c))
}

/// "tag_name" of GitHub's release JSON (quotes inside strings are escaped, so
/// the key can't match inside the release notes)
pub fn tag_name(json: &str) -> Option<String> {
    let mut rest = json;
    while let Some(i) = rest.find("\"tag_name\"") {
        rest = &rest[i + 10..];
        let Some(r) = rest.trim_start().strip_prefix(':') else { continue };
        let r = r.trim_start().strip_prefix('"')?;
        let end = r.find(['"', '\\'])?;
        let tag = &r[..end];
        return (r.as_bytes()[end] == b'"' && valid_tag(tag)).then(|| tag.to_string());
    }
    None
}

/// what a job ended with
enum Outcome {
    Tag(String), // the check: the latest release
    Saved(u64),  // the download: its size, in FILE
    Failed(String),
}

/// what the check or download thread reports
pub struct Shared {
    pub done: Mutex<u64>, // bytes downloaded (no 64-bit atomics on the ARM11)
    pub total: Mutex<u64>,
    seen: Mutex<u64>, // when bytes last came, for the stall watchdog
    sock: Mutex<Option<TcpStream>>,
    finished: AtomicBool,
    outcome: Mutex<Option<Outcome>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Check,
    Download,
}

struct Job {
    kind: Kind,
    shared: Arc<Shared>,
    thread: Option<sys::Joiner>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stage {
    Idle,
    Downloading,
    Waiting(u64), // downloaded (its size): installs once the SD card is free
    Installing,
    Done, // installed: used once Cement restarts
}

pub struct Updater {
    pub latest: Option<String>, // a newer release's tag
    pub stage: Stage,
    checked: bool, // the check ran since starting (or since it was turned back on)
    job: Option<Job>,
}

impl Updater {
    pub fn new() -> Updater {
        Updater { latest: None, stage: Stage::Idle, checked: false, job: None }
    }

    /// downloading, or downloaded and waiting to install
    pub fn downloading(&self) -> bool {
        matches!(self.stage, Stage::Downloading | Stage::Waiting(_))
    }

    /// bytes downloaded and the size, while downloading
    pub fn progress(&self) -> Option<(u64, u64)> {
        let job = self.job.as_ref().filter(|j| j.kind == Kind::Download)?;
        Some((*job.shared.done.lock().unwrap(), *job.shared.total.lock().unwrap()))
    }
}

fn why(f: Fail) -> String {
    match f {
        Fail::Connect => "no connection".into(),
        Fail::Cert => "the certificate isn't trusted".into(),
        Fail::Status(404) => "not found on GitHub".into(),
        Fail::Status(s) => format!("GitHub answered {s}"),
        Fail::Bad => "bad answer".into(),
        Fail::Io => "connection lost".into(),
    }
}

/// the check thread: the latest release's tag
fn check(url: &str, s: &Shared) -> Outcome {
    let mut body = match http::get(url, "application/vnd.github+json", &s.sock) {
        Ok(b) => b,
        Err(f) => return Outcome::Failed(why(f)),
    };
    let mut json = Vec::new();
    if body.by_ref().take(MAX_JSON).read_to_end(&mut json).is_err() {
        return Outcome::Failed("connection lost".into());
    }
    match tag_name(&String::from_utf8_lossy(&json)) {
        Some(tag) => Outcome::Tag(tag),
        None => Outcome::Failed("no release tag".into()),
    }
}

/// the download thread: the CIA streamed into FILE
fn download(url: &str, s: &Shared) -> Outcome {
    let mut body = match http::get(url, "application/octet-stream", &s.sock) {
        Ok(b) => b,
        Err(f) => return Outcome::Failed(why(f)),
    };
    let Some(total) = body.length.filter(|&n| n > 0 && n <= MAX_CIA) else {
        return Outcome::Failed("bad answer".into());
    };
    *s.total.lock().unwrap() = total;
    // the file and then the installed title
    if sys::sd_space().is_some_and(|(free, _)| free < 2 * total + (1 << 20)) {
        return Outcome::Failed("not enough free space".into());
    }
    sys::create_dir("/3ds"); // fail when they exist already
    sys::create_dir("/3ds/cement");
    let Some(f) = File::open_write(FILE).filter(|f| f.set_size(0)) else {
        return Outcome::Failed("SD card write error".into());
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut off = 0u64;
    loop {
        // whole buffers: fewer, bigger SD card writes
        let mut n = 0;
        while n < buf.len() {
            match body.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => {
                    n += k;
                    *s.seen.lock().unwrap() = sys::now_ms();
                }
                Err(_) => return Outcome::Failed("connection lost".into()),
            }
        }
        if n == 0 {
            break;
        }
        if f.write_at(off, &buf[..n]) != Some(n) {
            return Outcome::Failed("SD card write error".into());
        }
        off += n as u64;
        *s.done.lock().unwrap() = off;
    }
    if !f.flush() {
        return Outcome::Failed("SD card write error".into());
    }
    if off != total { Outcome::Failed("connection lost".into()) } else { Outcome::Saved(total) }
}

impl Server {
    fn update_spawn(&mut self, kind: Kind, url: String) -> bool {
        let shared = Arc::new(Shared {
            done: Mutex::new(0),
            total: Mutex::new(0),
            seen: Mutex::new(sys::now_ms()),
            sock: Mutex::new(None),
            finished: AtomicBool::new(false),
            outcome: Mutex::new(None),
        });
        let s = shared.clone();
        let f = Box::new(move || {
            let outcome = if kind == Kind::Check { check(&url, &s) } else { download(&url, &s) };
            *s.outcome.lock().unwrap() = Some(outcome);
            *s.sock.lock().unwrap() = None;
            s.finished.store(true, Ordering::Release);
        });
        match sys::spawn_low(STACK, f) {
            Some(t) => {
                self.update.job = Some(Job { kind, shared, thread: Some(t) });
                true
            }
            None => false,
        }
    }

    /// each loop: the check once the network is up, the jobs' ends, the install
    pub fn poll_update(&mut self) {
        let now = sys::now_ms();
        if let Some(job) = &mut self.update.job {
            if !job.shared.finished.load(Ordering::Acquire) {
                if now.saturating_sub(*job.shared.seen.lock().unwrap()) > STALL_MS {
                    http::cut(&job.shared.sock); // not every socket layer has timeouts
                }
            } else {
                if let Some(t) = job.thread.take() {
                    t.join();
                }
                let job = self.update.job.take().unwrap();
                let outcome = job.shared.outcome.lock().unwrap().take();
                self.update_ended(job.kind, outcome.unwrap_or(Outcome::Failed("stopped".into())));
            }
        }
        let u = &self.update;
        if self.prefs.update && !u.checked && u.job.is_none() && u.stage == Stage::Idle && self.net.up {
            self.update.checked = true;
            if let Some((api, _)) = sys::update_urls(API, DOWNLOAD) {
                self.update_spawn(Kind::Check, api);
            }
        }
        if let Stage::Waiting(size) = self.update.stage {
            if self.inst.is_none() && !self.fs_busy() {
                if self.install_file(FILE, size, false) {
                    self.update.stage = Stage::Installing;
                } else {
                    self.update.stage = Stage::Idle;
                    sys::delete_file(FILE);
                }
            }
        }
    }

    fn update_ended(&mut self, kind: Kind, outcome: Outcome) {
        match outcome {
            Outcome::Tag(tag) => {
                let own = sys::app_version();
                sys::debug(&format!("update check: {tag} (this is {own})"));
                if newer(&tag, &own) && self.prefs.update {
                    self.update.latest = Some(tag);
                    self.bottom_dirty = true;
                    self.last_time = 0; // the top screen may show it
                }
            }
            Outcome::Saved(size) => {
                sys::set_sleep_allowed(true); // the install blocks it again
                // only ever Cement
                if crate::install::cia_info(FILE).is_some_and(|i| i.id == TITLE) {
                    self.update.stage = Stage::Waiting(size);
                } else {
                    self.update_failed("that isn't Cement".into());
                }
            }
            Outcome::Failed(why) if kind == Kind::Download => {
                sys::set_sleep_allowed(true);
                self.update_failed(why);
            }
            Outcome::Failed(why) => sys::debug(&format!("update check failed: {why}")),
        }
    }

    fn update_failed(&mut self, why: String) {
        sys::debug(&format!("update failed: {why}"));
        sys::delete_file(FILE);
        self.update.stage = Stage::Idle;
        self.cia_message(format!("Update failed: {why}"));
    }

    /// the install of FILE ended (install.rs)
    pub fn update_installed(&mut self, ok: bool) {
        if ok {
            self.update.stage = Stage::Done;
            self.update.latest = None;
            self.cia_news("Updated: restart Cement".into());
        } else {
            self.update.stage = Stage::Idle;
            sys::delete_file(FILE); // the install keeps a file that failed
        }
    }

    /// the Updates row's switch: off forgets a found release, on checks again
    pub fn update_toggle(&mut self) {
        if self.update.stage != Stage::Idle && self.update.stage != Stage::Done {
            return;
        }
        self.prefs.update = !self.prefs.update;
        self.update.latest = None;
        self.update.checked = false;
        crate::settings::save_prefs(&self.prefs);
        self.bottom_dirty = true;
        self.last_time = 0;
    }

    /// A on the Updates row: installs a found release, else turns the check on or off
    pub fn update_activate(&mut self) {
        match self.update.stage {
            Stage::Idle => {}
            Stage::Done => return self.cia_message("Restart Cement to use the update".into()),
            _ => return,
        }
        let Some(tag) = self.update.latest.clone() else { return self.update_toggle() };
        let msg = if !self.net.up {
            Some(if self.prefs.ftp { "No Wi-Fi: try again later" } else { "Turn the FTP server on to update" })
        } else if battery_low(self.battery()) {
            Some("Battery too low: plug in the charger")
        } else if self.inst.is_some() {
            Some("Wait for the install to finish")
        } else {
            None
        };
        if let Some(msg) = msg {
            return self.cia_message(msg.into());
        }
        let Some((_, url)) = sys::update_urls(API, DOWNLOAD) else { return };
        self.cia_msg.clear();
        sys::set_sleep_allowed(false); // never sleep in the middle of it
        if self.update_spawn(Kind::Download, url.replace("{tag}", &tag)) {
            self.update.stage = Stage::Downloading;
            self.last_wake = sys::now_ms();
        } else {
            sys::set_sleep_allowed(true);
            self.cia_message("Could not start the update".into());
        }
        self.bottom_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.0"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.10.3"), Some((1, 10, 3)));
        assert_eq!(parse_version("1.1.0-4983859-dev"), Some((1, 1, 0)));
        assert_eq!(parse_version("dev"), None);
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.0.1"), None);
        assert_eq!(parse_version("v1.x.0"), None);
        assert!(newer("v1.2.0", "1.1.0"));
        assert!(newer("v1.10.0", "1.9.9"), "numbers, not text");
        assert!(newer("v2.0.0", "1.12.4"));
        assert!(!newer("v1.1.0", "1.1.0"));
        assert!(!newer("v1.1.0", "1.1.0-4983859-dev"), "a dev build counts as its base version");
        assert!(newer("v1.1.1", "1.1.0-4983859-dev"));
        assert!(!newer("v1.0.9", "1.1.0"));
        assert!(!newer("v1.2.0", "dev"), "a build without a version never updates");
        assert!(!newer("latest", "1.1.0"));
    }

    #[test]
    fn tags() {
        let json = r#"{"url":"https://api.github.com/x","tag_name" : "v1.2.0","name":"1.2.0","body":"see \"tag_name\": \"v9.9.9\""}"#;
        assert_eq!(tag_name(json).as_deref(), Some("v1.2.0"));
        assert_eq!(
            tag_name(r#"{"body":"a \"tag_name\":\"v9.0.0\" b","tag_name":"v1.3.0"}"#).as_deref(),
            Some("v1.3.0")
        );
        assert_eq!(tag_name("{\n  \"tag_name\":\n  \"v1.0.0\"\n}").as_deref(), Some("v1.0.0"));
        assert_eq!(tag_name(r#"{"message":"Not Found"}"#), None);
        assert_eq!(tag_name(r#"{"tag_name":"v1/../../x"}"#), None, "only characters a URL takes as they are");
        assert_eq!(tag_name(r#"{"tag_name":"v1\"2"}"#), None);
        assert_eq!(tag_name(r#"{"tag_name":12}"#), None);
        assert_eq!(tag_name(r#"{"tag_name":"v1.2.0"#), None, "cut short");
    }
}
