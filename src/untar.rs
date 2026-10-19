// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// SITE BUNDLE: a plain (uncompressed) ustar stream unpacked as it arrives.
// It runs on the disk thread: the transfer hands it each received buffer in
// order, and it creates the folders and writes the files itself. Regular
// files, folders, the ustar prefix, PAX 'x' path/size records and GNU 'L' long
// names are understood; other entries are skipped. Absolute paths and ".."
// are refused (the entry is skipped and counted as an error).

use std::collections::HashSet;

use crate::sys::{self, File};
use crate::util::{is_cia, resolve_path};

#[derive(PartialEq)]
enum State {
    Header,
    Data,       // a regular file's bytes (written when `file` is open)
    Skip,       // bytes of an entry we don't keep, or padding
    Meta(bool), // PAX extended header (false) or GNU long name (true)
    End,        // the two zero blocks were seen: anything after is ignored
}

pub struct Untar {
    dir: String, // where to unpack, an absolute SD card path
    state: State,
    hdr: [u8; 512],
    have: usize, // bytes of `hdr` filled
    left: u64,   // bytes left in the current entry (or to skip)
    pad: u64,    // padding after the entry's data
    meta: Vec<u8>,
    next_path: Option<String>, // from a PAX or GNU long name record
    next_size: Option<u64>,
    file: Option<File>,
    path: String, // the file being written (full SD path)
    off: u64,
    size: u64,
    zeros: u32,
    made: HashSet<String>, // folders created (or found) already
    pub current: String,   // archive name of the entry being unpacked, for the screens
    pub files: u32,
    pub errors: u32,
    pub first_error: String,
    pub cias: Vec<(String, u64)>, // unpacked CIA files, for the bottom screen
}

/// an octal number field (or GNU base-256 for big sizes)
fn number(field: &[u8]) -> u64 {
    if field[0] & 0x80 != 0 {
        return field[1..].iter().fold(0u64, |v, &b| v << 8 | b as u64);
    }
    field
        .iter()
        .skip_while(|&&c| c == b' ')
        .take_while(|&&c| (b'0'..=b'7').contains(&c))
        .fold(0u64, |v, &c| v * 8 + (c - b'0') as u64)
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&c| c == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

impl Untar {
    pub fn new(dir: &str) -> Untar {
        Untar {
            dir: dir.to_string(),
            state: State::Header,
            hdr: [0; 512],
            have: 0,
            left: 0,
            pad: 0,
            meta: Vec::new(),
            next_path: None,
            next_size: None,
            file: None,
            path: String::new(),
            off: 0,
            size: 0,
            zeros: 0,
            made: HashSet::new(),
            current: String::new(),
            files: 0,
            errors: 0,
            first_error: String::new(),
            cias: Vec::new(),
        }
    }

    /// the stream ended between two entries (or after the end marker)
    pub fn clean_end(&self) -> bool {
        self.state == State::End || (self.state == State::Header && self.have == 0)
    }

    /// the stream was cut: the file being written is incomplete, remove it
    pub fn abort(&mut self) {
        if self.file.take().is_some() {
            sys::delete_file(&self.path);
        }
        self.state = State::End;
    }

    fn error(&mut self, name: &str) {
        self.errors += 1;
        if self.first_error.is_empty() {
            self.first_error = name.to_string();
        }
    }

    /// the full SD path of an archive name, None for an absolute path or ".."
    fn target(&self, name: &str) -> Option<String> {
        let name = name.trim_end_matches('/');
        if name.is_empty() || name.starts_with('/') || name.contains('\\') {
            return None;
        }
        let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
        if parts.is_empty() || parts.contains(&"..") {
            return None;
        }
        let full = resolve_path(&self.dir, parts.join("/").as_bytes())?;
        (full.starts_with(&self.dir) && full.len() > self.dir.len()).then_some(full)
    }

    /// creates `path` and its parents (each folder tried once per bundle)
    fn make_dirs(&mut self, path: &str) {
        let mut at = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            at.push('/');
            at.push_str(part);
            if self.made.insert(at.clone()) {
                sys::create_dir(&at); // fails when it exists: fine
            }
        }
    }

    fn finish_file(&mut self) {
        if let Some(f) = self.file.take() {
            f.flush();
            self.files += 1;
            if is_cia(&self.path) {
                self.cias.push((self.path.clone(), self.size));
            }
        }
    }

    /// after an entry's data: its padding, then the next header
    fn after_entry(&mut self) {
        self.left = self.pad;
        self.state = if self.left == 0 { State::Header } else { State::Skip };
    }

    fn header(&mut self) {
        let h = self.hdr;
        if h.iter().all(|&b| b == 0) {
            self.zeros += 1;
            if self.zeros >= 2 {
                self.state = State::End;
            }
            return;
        }
        self.zeros = 0;
        // the checksum counts its own field as spaces
        let sum: u64 = h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as u64 }).sum();
        if sum != number(&h[148..156]) {
            self.error("(damaged archive)");
            self.state = State::End;
            return;
        }
        let size = self.next_size.take().unwrap_or_else(|| number(&h[124..136]));
        let name = self.next_path.take().unwrap_or_else(|| {
            let (name, prefix) = (text(&h[0..100]), text(&h[345..500]));
            if &h[257..262] == b"ustar" && !prefix.is_empty() { format!("{prefix}/{name}") } else { name }
        });
        self.pad = (512 - size % 512) % 512;
        self.left = size;
        match h[156] {
            b'x' | b'L' => {
                self.meta.clear();
                self.state = State::Meta(h[156] == b'L');
            }
            b'0' | 0 => {
                self.current = name.clone();
                let Some(path) = self.target(&name) else {
                    self.error(&name);
                    self.state = State::Skip;
                    self.left = size + self.pad;
                    return;
                };
                if let Some(parent) = path.rfind('/').map(|at| path[..at].to_string()) {
                    self.make_dirs(&parent);
                }
                match File::open_write(&path) {
                    Some(f) if f.set_size(0) => {
                        self.file = Some(f);
                        self.path = path;
                        self.off = 0;
                        self.size = size;
                        if size == 0 {
                            self.finish_file();
                            self.after_entry();
                        } else {
                            self.state = State::Data;
                        }
                    }
                    _ => {
                        self.error(&name);
                        self.state = State::Data; // its bytes go nowhere
                    }
                }
            }
            b'5' => {
                match self.target(&name) {
                    Some(path) => self.make_dirs(&path),
                    None => self.error(&name),
                }
                self.left += self.pad;
                self.state = if self.left == 0 { State::Header } else { State::Skip };
            }
            _ => {
                self.left += self.pad;
                self.state = if self.left == 0 { State::Header } else { State::Skip };
            }
        }
    }

    /// PAX records "<len> <key>=<value>\n": path and size are used
    fn pax(&mut self) {
        let mut at = 0;
        while at < self.meta.len() {
            let Some(sp) = self.meta[at..].iter().position(|&c| c == b' ') else { break };
            let len: usize = std::str::from_utf8(&self.meta[at..at + sp]).ok().and_then(|n| n.parse().ok()).unwrap_or(0);
            if len <= sp || at + len > self.meta.len() {
                break;
            }
            let record = &self.meta[at + sp + 1..at + len];
            let record = record.strip_suffix(b"\n").unwrap_or(record);
            if let Some(eq) = record.iter().position(|&c| c == b'=') {
                let value = String::from_utf8_lossy(&record[eq + 1..]).into_owned();
                match &record[..eq] {
                    b"path" => self.next_path = Some(value),
                    b"size" => self.next_size = value.parse().ok(),
                    _ => {}
                }
            }
            at += len;
        }
    }

    /// the next bytes of the stream
    pub fn feed(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            match self.state {
                State::End => return,
                State::Header => {
                    let n = (512 - self.have).min(data.len());
                    self.hdr[self.have..self.have + n].copy_from_slice(&data[..n]);
                    self.have += n;
                    data = &data[n..];
                    if self.have == 512 {
                        self.have = 0;
                        self.header();
                    }
                }
                State::Data => {
                    let n = (self.left.min(data.len() as u64)) as usize;
                    if let Some(f) = &self.file {
                        if f.write_at(self.off, &data[..n]) != Some(n) {
                            self.file = None;
                            sys::delete_file(&self.path);
                            let name = self.current.clone();
                            self.error(&name);
                        }
                    }
                    self.off += n as u64;
                    self.left -= n as u64;
                    data = &data[n..];
                    if self.left == 0 {
                        self.finish_file();
                        self.after_entry();
                    }
                }
                State::Skip => {
                    let n = self.left.min(data.len() as u64) as usize;
                    self.left -= n as u64;
                    data = &data[n..];
                    if self.left == 0 {
                        self.state = State::Header;
                    }
                }
                State::Meta(long) => {
                    let n = self.left.min(data.len() as u64) as usize;
                    if self.meta.len() < 64 * 1024 {
                        self.meta.extend_from_slice(&data[..n]);
                    }
                    self.left -= n as u64;
                    data = &data[n..];
                    if self.left == 0 {
                        if long {
                            self.next_path = Some(text(&self.meta));
                        } else {
                            self.pax();
                        }
                        self.after_entry();
                    }
                }
            }
        }
    }
}
