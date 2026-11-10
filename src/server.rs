// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: AGPL-3.0-or-later
// FTP sessions: commands, data transfers and the network loop.
//
// One non-blocking poll() loop on the main thread handles every socket; the
// disk thread does the SD card reads and writes of the transfers (disk.rs).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use crate::disk::{self, Buf, CHUNK, Disk, NBUF};
use crate::install::InstallStart;
use crate::sys::net::{self, Io, POLL_IN, POLL_OUT, PollFd, Sock};
use crate::sys::{self, File};
use crate::untar::Untar;
use crate::util::{self, resolve_path};
use crate::{PROTOCOL, Server, VERSION, settings};

pub const CTRL_PORT: u16 = 5000; // on the 3DS; the PC test server can move it (sys::ports)
pub const MAX_SESSIONS: usize = 4;
const MAX_PENDING: usize = 16;
const SOCK_BUFSIZE: i32 = 32768; // same value ftpd uses on 3DS
const CMD_SIZE: usize = 4096;
const DATA_TIMEOUT: u64 = 15_000; // ms to wait for the data connection
const IDLE_TIMEOUT: u64 = 300_000; // ms without any command before dropping a client
const WRONG_PASS_MS: u64 = 1000; // after a wrong password, no other is checked for this long
const MAX_WRONG_PASS: u8 = 3; // wrong passwords before a connection is closed
const MAX_OUT: usize = 64 * 1024; // unsent replies before a client is dropped

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    None,
    List,
    Mlsd,
    Nlst,
    Stor,
    Retr,
}

/// one transfer buffer and who has it
enum Slot {
    Empty, // not allocated
    Free(Buf),
    Fill { buf: Buf, len: usize, off: u64 }, // STOR: being received into
    Busy,                                    // at the disk thread
    Done { buf: Buf, len: usize, ok: bool }, // back from the disk thread
}

pub struct Xfer {
    file: Option<Arc<File>>,
    untar: Option<Arc<Mutex<Untar>>>, // SITE BUNDLE: the stream is unpacked instead of written
    up: bool,                         // network -> SD
    slots: [Slot; NBUF],
    cur: usize,      // STOR: buffer being filled, RETR: buffer being sent
    send_pos: usize, // RETR: bytes of the current buffer already sent
    pub pos: u64,    // STOR: file offset of the next byte, RETR: next offset to read
    pub size: u64,   // RETR: file size
    pub done: u64,   // bytes moved over the network
    err: bool,
    tx: Sender<disk::Done>,
    rx: Receiver<disk::Done>,
}

impl Xfer {
    fn new() -> Xfer {
        let (tx, rx) = channel();
        Xfer {
            file: None,
            untar: None,
            up: false,
            slots: [const { Slot::Empty }; NBUF],
            cur: 0,
            send_pos: 0,
            pos: 0,
            size: 0,
            done: 0,
            err: false,
            tx,
            rx,
        }
    }

    /// buffers the disk thread has finished with
    fn collect(&mut self) {
        while let Ok(d) = self.rx.try_recv() {
            self.slots[d.slot] = Slot::Done { buf: d.buf, len: d.len, ok: d.ok };
        }
    }

    fn busy(&self, i: usize) -> bool {
        matches!(self.slots[i], Slot::Busy)
    }

    fn has_buffers(&self) -> bool {
        !matches!(self.slots[0], Slot::Empty)
    }

    fn alloc(&mut self) -> bool {
        for slot in &mut self.slots {
            if matches!(slot, Slot::Empty) {
                match Buf::new() {
                    Some(b) => *slot = Slot::Free(b),
                    None => return false,
                }
            }
        }
        true
    }

    /// hands buffer i to the disk thread: write its `len` bytes, or read a chunk, at `off`
    fn submit(&mut self, disk: &Disk, i: usize, len: usize, off: u64) {
        if self.file.is_none() && self.untar.is_none() {
            return;
        }
        let buf = match std::mem::replace(&mut self.slots[i], Slot::Busy) {
            Slot::Free(b) | Slot::Fill { buf: b, .. } | Slot::Done { buf: b, .. } => b,
            other => {
                self.slots[i] = other;
                return;
            }
        };
        let (file, untar) = (self.file.clone(), self.untar.clone());
        disk.submit(disk::Job { file, untar, write: self.up, slot: i, buf, len, off, reply: self.tx.clone() });
    }

    /// waits until the disk thread is done with every buffer of this transfer
    fn drain(&mut self) {
        while self.slots.iter().any(|s| matches!(s, Slot::Busy)) {
            match self.rx.recv() {
                Ok(d) => self.slots[d.slot] = Slot::Done { buf: d.buf, len: d.len, ok: d.ok },
                Err(_) => break,
            }
        }
        for slot in &mut self.slots {
            *slot = match std::mem::replace(slot, Slot::Empty) {
                Slot::Done { buf, ok, .. } => {
                    self.err |= !ok;
                    Slot::Free(buf)
                }
                Slot::Fill { buf, .. } | Slot::Free(buf) => Slot::Free(buf),
                other => other,
            };
        }
    }

    /// ready for a new transfer: every buffer free again, nothing done
    fn reset(&mut self, up: bool) {
        for slot in &mut self.slots {
            *slot = match std::mem::replace(slot, Slot::Empty) {
                Slot::Fill { buf, .. } | Slot::Done { buf, .. } | Slot::Free(buf) => Slot::Free(buf),
                other => other,
            };
        }
        self.cur = 0;
        self.send_pos = 0;
        self.done = 0;
        self.err = false;
        self.up = up;
    }

    fn free_buffers(&mut self) {
        self.drain();
        for slot in &mut self.slots {
            *slot = Slot::Empty;
        }
    }
}

pub struct Session {
    pub ctrl: Option<Sock>,
    pasv: Option<Sock>,
    data: Option<Sock>,
    pub peer: u32, // client IP: one PC opens several connections
    cmd: Vec<u8>,
    out: Vec<u8>, // replies the control socket hasn't taken yet
    cwd: String,
    rnfr: String,
    rest: u64,
    last_cmd: u64,
    data_since: u64,

    pub mode: Mode, // current/pending data transfer
    started: bool,
    pub name: String, // file name shown on screen
    pub path: String, // full path of the current file transfer

    list: Vec<u8>,
    list_pos: usize,

    pub x: Xfer,

    logged_in: bool,        // USER/PASS matched the login (always true without one)
    user: String,           // the USER given, waiting for PASS
    wrong: u8,              // wrong passwords on this connection
    bundle: Option<String>, // SITE BUNDLE: the next STOR is a tar unpacked into this folder
    bundle_base: u32,       // batch_files_done when the bundle started

    // progress reporting (SITE STATUS)
    allo: u64,       // size announced with ALLO for the next STOR (0 = unknown)
    xfer_base: u64,  // REST offset of the current transfer
    xfer_total: u64, // full size of the current file (0 = unknown)
    xfer_start: u64, // when the data started flowing

    // whole job announced with SITE BATCH <files> <bytes> <kind>: busy between files, ETA
    pub batch_kind: BatchKind,
    pub batch_files: u32,
    pub batch_files_done: u32,
    batch_bytes: u64,
    batch_bytes_done: u64,
    batch_start: u64,
}

impl Session {
    pub fn new() -> Session {
        Session {
            ctrl: None,
            pasv: None,
            data: None,
            peer: 0,
            cmd: Vec::new(),
            out: Vec::new(),
            cwd: "/".into(),
            rnfr: String::new(),
            rest: 0,
            last_cmd: 0,
            data_since: 0,
            mode: Mode::None,
            started: false,
            name: String::new(),
            path: String::new(),
            list: Vec::new(),
            list_pos: 0,
            x: Xfer::new(),
            logged_in: false,
            user: String::new(),
            wrong: 0,
            bundle: None,
            bundle_base: 0,
            allo: 0,
            xfer_base: 0,
            xfer_total: 0,
            xfer_start: 0,
            batch_kind: BatchKind::Up,
            batch_files: 0,
            batch_files_done: 0,
            batch_bytes: 0,
            batch_bytes_done: 0,
            batch_start: 0,
        }
    }

    /// a file transfer, or a job between its files
    pub fn working(&self) -> bool {
        self.ctrl.is_some() && (self.mode == Mode::Stor || self.mode == Mode::Retr || self.batch_files != 0)
    }

    /// queued after any earlier reply and sent as far as the socket takes it
    /// now; the rest goes when the socket can take more (serve), so a slow
    /// client never holds up the main loop
    pub fn reply(&mut self, text: &str) {
        if self.ctrl.is_none() {
            return;
        }
        self.out.extend_from_slice(text.as_bytes());
        self.send_out();
    }

    fn send_out(&mut self) {
        let Some(ctrl) = self.ctrl else { return };
        while !self.out.is_empty() {
            match net::send(ctrl, &self.out) {
                Io::Done(n) => {
                    self.out.drain(..n);
                }
                Io::WouldBlock => break,
                _ => {
                    self.out.clear();
                    close_now(&mut self.ctrl);
                    return;
                }
            }
        }
        // a client that stopped reading its replies
        if self.out.len() > MAX_OUT {
            self.out.clear();
            close_now(&mut self.ctrl);
        }
    }

    fn xfer_close(&mut self) {
        if self.x.file.is_none() && self.x.untar.is_none() {
            return;
        }
        self.x.drain();
        if let Some(f) = self.x.file.take()
            && self.x.up
            && !f.flush()
        {
            self.x.err = true; // the upload may not be complete on the card
        }
    }

    fn end_transfer(&mut self) {
        self.xfer_close();
        if let Some(u) = self.x.untar.take() {
            u.lock().unwrap().abort(); // cut short (ABOR, lost client): no half file left
        }
        self.list = Vec::new();
        self.list_pos = 0;
        self.mode = Mode::None;
        self.started = false;
        self.name.clear();
        self.x.done = 0;
    }

    pub fn close(&mut self) {
        self.end_transfer();
        self.logged_in = false;
        self.user.clear();
        self.wrong = 0;
        self.out.clear();
        self.bundle = None;
        self.allo = 0;
        self.batch_files = 0;
        self.batch_files_done = 0;
        self.batch_bytes = 0;
        self.batch_bytes_done = 0;
        close_now(&mut self.data);
        close_now(&mut self.pasv);
        close_now(&mut self.ctrl);
        self.x.free_buffers();
    }

    fn set_name(&mut self, path: &str) {
        self.path = path.to_string();
        self.name = util::clip(util::base_name(path), 63).to_string();
    }

    fn list_entry(&mut self, mode: Mode, name: &str, dir: bool, size: u64) {
        let line = match mode {
            Mode::Mlsd => format!("type={};size={}; {}\r\n", if dir { "dir" } else { "file" }, size, name),
            Mode::Nlst => format!("{name}\r\n"),
            _ => {
                format!("{} 1 3ds 3ds {} Jan 01 2000 {}\r\n", if dir { "drwxrwxrwx" } else { "-rw-rw-rw-" }, size, name)
            }
        };
        self.list.extend_from_slice(line.as_bytes());
    }
}

fn close_now(fd: &mut Option<Sock>) {
    if let Some(s) = fd.take() {
        net::close(s);
    }
}

/// SITE STATUS numbers for one session
pub struct Progress {
    pub active: bool, // a file transfer is flowing right now
    pub done: u64,
    pub size: u64, // current file (size 0 = unknown)
    pub bps: u64,
    pub eta: i64, // seconds, -1 = unknown
    pub batch_done: u64,
    pub batch_bytes: u64, // whole announced job
    pub batch_eta: i64,
}

pub fn progress_of(s: &Session, now: u64) -> Progress {
    let mut p =
        Progress { active: false, done: 0, size: 0, bps: 0, eta: -1, batch_done: 0, batch_bytes: 0, batch_eta: -1 };
    p.active = s.started && (s.mode == Mode::Stor || s.mode == Mode::Retr);
    if p.active {
        let ms = now.saturating_sub(s.xfer_start);
        p.done = s.xfer_base + s.x.done;
        p.size = s.xfer_total;
        p.bps = if ms >= 500 { s.x.done * 1000 / ms } else { 0 };
        if p.size >= p.done && p.bps != 0 {
            p.eta = ((p.size - p.done) / p.bps) as i64;
        }
    }
    if s.batch_files != 0 {
        let ms = now.saturating_sub(s.batch_start);
        p.batch_done = s.batch_bytes_done + if p.active { s.x.done } else { 0 };
        p.batch_bytes = s.batch_bytes;
        let bps = if ms >= 500 { p.batch_done * 1000 / ms } else { 0 };
        if p.batch_bytes >= p.batch_done && bps != 0 {
            p.batch_eta = ((p.batch_bytes - p.batch_done) / bps) as i64;
        } else if s.batch_kind == BatchKind::Delete && s.batch_files_done != 0 && ms >= 500 {
            p.batch_eta = ((s.batch_files - s.batch_files_done) as u64 * ms / s.batch_files_done as u64 / 1000) as i64;
        }
    }
    p
}

/// what a SITE BATCH job does, as SITE STATUS reports it
#[derive(Clone, Copy, PartialEq)]
pub enum BatchKind {
    Up,
    Down,
    Delete,
}

impl BatchKind {
    /// "up" when not one of the three
    fn parse(word: &str) -> BatchKind {
        match word {
            "down" => BatchKind::Down,
            "delete" => BatchKind::Delete,
            _ => BatchKind::Up,
        }
    }

    fn name(self) -> &'static str {
        match self {
            BatchKind::Up => "up",
            BatchKind::Down => "down",
            BatchKind::Delete => "delete",
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Listen,
    Ctrl(usize),
    Pasv(usize),
    Data(usize),
    Pending(usize),
}

pub struct Net {
    pub listen: Option<Sock>,
    pending: [Option<(Sock, u64)>; MAX_PENDING], // closing data connections and their deadline
    pub up: bool,
    ndm_locked: bool,
    pub ip: u32, // s_addr
    pasv_port: u16,
}

impl Net {
    pub fn new() -> Net {
        Net {
            listen: None,
            pending: [None; MAX_PENDING],
            up: false,
            ndm_locked: false,
            ip: 0,
            pasv_port: sys::ports().1,
        }
    }

    /// half-close and let the peer close, so no queued data gets lost
    fn close_graceful(&mut self, fd: &mut Option<Sock>) {
        let Some(s) = fd.take() else { return };
        net::shutdown_write(s);
        match self.pending.iter_mut().find(|p| p.is_none()) {
            Some(slot) => *slot = Some((s, sys::now_ms() + 3000)),
            None => net::close(s),
        }
    }
}

// ---------------------------------------------------------------------------

impl Server {
    // network lifecycle

    pub fn net_down(&mut self) {
        for s in &mut self.sess {
            s.close();
        }
        for p in &mut self.net.pending {
            if let Some((s, _)) = p.take() {
                net::close(s);
            }
        }
        close_now(&mut self.net.listen);
        if self.net.ndm_locked {
            sys::ndm_leave();
            self.net.ndm_locked = false;
        }
        if self.net.up {
            sys::soc_exit();
        }
        self.net.up = false;
        self.net.ip = 0;
    }

    pub fn net_up(&mut self) {
        if !sys::wifi_connected() {
            self.status = "Waiting for Wi-Fi...".into();
            return;
        }
        if !sys::soc_init() {
            self.status = "socInit failed, retrying...".into();
            return;
        }
        self.net.up = true;
        self.net.ip = sys::host_ip();
        // keep StreetPass/SpotPass from using the Wi-Fi radio while we serve
        self.net.ndm_locked = sys::ndm_enter();

        let listen = net::tcp_socket();
        if let Some(s) = listen {
            // after waking up, the old connections may still hold the port (TIME_WAIT):
            // without this the server could not listen again for a while (ftpd does the same)
            net::set_reuse_addr(s);
        }
        self.net.listen = listen;
        let ip = self.net.ip;
        let ok = ip != 0
            && ip != u32::MAX
            && listen.is_some_and(|s| net::bind(s, ip, sys::ports().0) && net::listen(s, MAX_SESSIONS as i32));
        if !ok {
            self.net_down();
            self.status = "Network not ready, retrying...".into();
            return;
        }
        net::set_nonblocking(listen.unwrap());
        self.status = "Ready".into();
    }

    fn accept_client(&mut self) {
        let Some(listen) = self.net.listen else { return };
        let Some((fd, peer)) = net::accept(listen) else { return };
        match self.sess.iter_mut().find(|s| s.ctrl.is_none()) {
            Some(s) => {
                s.ctrl = Some(fd);
                s.peer = peer;
                net::set_nonblocking(fd);
                net::set_nodelay(fd);
                s.cmd.clear();
                s.rest = 0;
                s.rnfr.clear();
                s.last_cmd = sys::now_ms();
                s.cwd = "/".into();
                s.reply("220 Cement ready\r\n");
            }
            None => {
                let _ = net::send(fd, b"421 Too many connections\r\n");
                net::close(fd);
            }
        }
    }

    /// one iteration of the network loop
    pub fn serve(&mut self) {
        let (mut fds, kinds, disk_wait) = self.poll_set(sys::now_ms());
        let rc = net::poll(&mut fds, if disk_wait { 1 } else { 30 });
        if rc < 0 {
            // the network went away underneath us
            self.net_down();
            return;
        }
        if rc == 0 {
            return;
        }

        for (p, kind) in fds.iter().zip(&kinds) {
            if p.revents == 0 {
                continue;
            }
            match *kind {
                Kind::Listen => self.accept_client(),
                Kind::Ctrl(i) => {
                    if self.sess[i].ctrl == Some(p.fd) && p.revents & POLL_OUT != 0 {
                        self.sess[i].send_out();
                    }
                    if self.sess[i].ctrl == Some(p.fd) && p.revents & !POLL_OUT != 0 {
                        self.read_commands(i);
                    }
                }
                Kind::Pasv(i) => {
                    if self.sess[i].pasv == Some(p.fd) {
                        self.accept_data(i, p.fd);
                    }
                }
                Kind::Data(i) => {
                    let s = &self.sess[i];
                    if s.data != Some(p.fd) || s.ctrl.is_none() {
                        continue;
                    }
                    if !s.started {
                        self.start_data(i);
                    }
                    match self.sess[i].mode {
                        Mode::Stor => self.pump_stor(i),
                        Mode::Retr => self.pump_retr(i),
                        Mode::None => {}
                        _ => self.pump_list(i),
                    }
                }
                Kind::Pending(j) => {
                    if let Some((fd, _)) = self.net.pending[j] {
                        let mut junk = [0u8; 256];
                        if fd == p.fd && !matches!(net::recv(fd, &mut junk), Io::Done(_)) {
                            net::close(fd);
                            self.net.pending[j] = None;
                        }
                    }
                }
            }
        }
    }

    /// the sockets to wait on and what each one is, dropping idle and dead
    /// connections on the way; true when a transfer waits for the disk thread
    /// (then the wait is short)
    fn poll_set(&mut self, now: u64) -> (Vec<PollFd>, Vec<Kind>, bool) {
        let mut fds: Vec<PollFd> = Vec::with_capacity(1 + MAX_SESSIONS * 2 + MAX_PENDING);
        let mut kinds: Vec<Kind> = Vec::with_capacity(fds.capacity());
        let mut disk_wait = false;
        let mut add = |fd: Sock, events: i16, kind: Kind| {
            fds.push(PollFd { fd, events, revents: 0 });
            kinds.push(kind);
        };

        if let Some(l) = self.net.listen {
            add(l, POLL_IN, Kind::Listen);
        }
        for i in 0..MAX_SESSIONS {
            let s = &mut self.sess[i];
            let Some(ctrl) = s.ctrl else {
                // control connection died while replying: release the rest
                if s.data.is_some() || s.pasv.is_some() || s.x.file.is_some() || !s.list.is_empty() || s.x.has_buffers()
                {
                    s.close();
                }
                continue;
            };
            if s.mode == Mode::None && now.saturating_sub(s.last_cmd) > IDLE_TIMEOUT {
                s.close();
                continue;
            }
            add(ctrl, if s.out.is_empty() { POLL_IN } else { POLL_IN | POLL_OUT }, Kind::Ctrl(i));
            if let Some(pasv) = s.pasv {
                if now.saturating_sub(s.data_since) > DATA_TIMEOUT {
                    close_now(&mut s.pasv);
                    if s.mode != Mode::None {
                        self.finish_data(i, false, "425 No data connection");
                    }
                } else {
                    add(pasv, POLL_IN, Kind::Pasv(i));
                }
            }
            let s = &mut self.sess[i];
            if let (Some(data), true) = (s.data, s.mode != Mode::None) {
                if !s.started {
                    self.start_data(i);
                }
                let s = &mut self.sess[i];
                s.x.collect();
                let cur = s.x.cur;
                if s.mode == Mode::Stor {
                    if s.x.busy(cur) {
                        disk_wait = true;
                    } else {
                        add(data, POLL_IN, Kind::Data(i));
                    }
                } else if s.mode == Mode::Retr && s.x.busy(cur) {
                    disk_wait = true;
                } else {
                    add(data, POLL_OUT, Kind::Data(i));
                }
            }
        }
        for (j, p) in self.net.pending.iter_mut().enumerate() {
            if let Some((fd, deadline)) = *p {
                if now > deadline {
                    net::close(fd);
                    *p = None;
                } else {
                    add(fd, POLL_IN, Kind::Pending(j));
                }
            }
        }
        (fds, kinds, disk_wait)
    }

    /// a data connection on the passive port, taken only from the client's own
    /// address: anyone else could take the next port and the data with it
    fn accept_data(&mut self, i: usize, listen: Sock) {
        let s = &mut self.sess[i];
        let Some((fd, peer)) = net::accept(listen) else { return };
        if peer != s.peer {
            net::close(fd);
            return;
        }
        close_now(&mut s.pasv);
        close_now(&mut s.data);
        s.data = Some(fd);
        net::set_nonblocking(fd);
        net::set_buffers(fd, SOCK_BUFSIZE);
    }

    fn read_commands(&mut self, i: usize) {
        loop {
            let Some(ctrl) = self.sess[i].ctrl else { return };
            let mut buf = std::mem::take(&mut self.sess[i].cmd);
            let room = CMD_SIZE - 1 - buf.len();
            let start = buf.len();
            buf.resize(start + room, 0);
            match net::recv(ctrl, &mut buf[start..]) {
                Io::Done(n) => buf.truncate(start + n),
                Io::WouldBlock => {
                    buf.truncate(start);
                    self.sess[i].cmd = buf;
                    return;
                }
                Io::Closed | Io::Failed => {
                    self.sess[i].close();
                    return;
                }
            }

            // strip telnet IAC sequences (sent by some clients before ABOR)
            let mut w = 0;
            let mut r = 0;
            while r < buf.len() {
                if buf[r] == 0xFF {
                    r += 2;
                    continue;
                }
                buf[w] = buf[r];
                w += 1;
                r += 1;
            }
            buf.truncate(w);

            let mut from = 0;
            while self.sess[i].ctrl.is_some() {
                let Some(nl) = buf[from..].iter().position(|&c| c == b'\n') else { break };
                let mut line = &buf[from..from + nl];
                if line.last() == Some(&b'\r') {
                    line = &line[..line.len() - 1];
                }
                let line = line.to_vec();
                from += nl + 1;
                self.handle_command(i, &line);
            }
            if self.sess[i].ctrl.is_none() {
                return;
            }
            buf.drain(..from);
            if buf.len() >= CMD_SIZE - 1 {
                buf.clear(); // line too long
            }
            self.sess[i].cmd = buf;
        }
    }

    fn open_pasv(&mut self, i: usize, extended: bool) {
        let ip = self.net.ip;
        let s = &mut self.sess[i];
        close_now(&mut s.data);
        close_now(&mut s.pasv);
        let Some(fd) = net::tcp_socket() else {
            s.reply("425 Can't open data socket\r\n");
            return;
        };
        net::set_buffers(fd, SOCK_BUFSIZE);
        net::set_reuse_addr(fd); // ports of earlier transfers may still be in TIME_WAIT

        // binding to port 0 is unreliable on 3DS (ftpd works around it the same way)
        let mut port = 0;
        for _ in 0..20 {
            let (_, first, last) = sys::ports();
            if self.net.pasv_port > last || self.net.pasv_port < first {
                self.net.pasv_port = first;
            }
            let p = self.net.pasv_port;
            self.net.pasv_port += 1;
            if net::bind(fd, ip, p) {
                port = p;
                break;
            }
        }
        let s = &mut self.sess[i];
        if port == 0 || !net::listen(fd, 1) {
            net::close(fd);
            s.reply("425 Can't open data socket\r\n");
            return;
        }
        net::set_nonblocking(fd);
        s.pasv = Some(fd);
        s.data_since = sys::now_ms();
        let a = ip.to_ne_bytes();
        if extended {
            s.reply(&format!("229 Entering Extended Passive Mode (|||{port}|)\r\n"));
        } else {
            s.reply(&format!(
                "227 Entering Passive Mode ({},{},{},{},{},{})\r\n",
                a[0],
                a[1],
                a[2],
                a[3],
                port >> 8,
                port & 0xFF
            ));
        }
    }

    /// false when the transfer could not be prepared (reply already sent)
    fn prepare_file(&mut self, i: usize, path: &str, mode: Mode, append: bool) -> bool {
        if !self.sess[i].x.alloc() {
            self.sess[i].reply("451 Out of memory\r\n");
            return false;
        }
        let up = mode == Mode::Stor;
        if up {
            self.cia_remove(path);
        }
        let s = &mut self.sess[i];
        let x = &mut s.x;
        x.reset(up);

        if up {
            let Some(file) = File::open_write(path) else {
                s.reply("550 Can't create file\r\n");
                return false;
            };
            let size = file.size().unwrap_or(0);
            let start = if append { size } else { s.rest };
            if start > size || !file.set_size(start) {
                s.reply("554 Invalid restart position\r\n");
                return false;
            }
            x.file = Some(Arc::new(file));
            x.pos = start;
            s.xfer_base = start;
            s.xfer_total = s.allo;
            s.allo = 0;
        } else {
            let Some(file) = File::open_read(path) else {
                s.reply("550 Can't open file\r\n");
                return false;
            };
            x.size = file.size().unwrap_or(0);
            if s.rest > x.size {
                s.reply("554 Invalid restart position\r\n");
                return false;
            }
            x.file = Some(Arc::new(file));
            x.pos = s.rest;
            s.xfer_base = s.rest;
            s.xfer_total = x.size;
        }
        true
    }

    fn start_data(&mut self, i: usize) {
        let s = &mut self.sess[i];
        s.started = true;
        s.xfer_start = sys::now_ms();
        if s.mode == Mode::Retr {
            // read ahead into every buffer
            let x = &mut s.x;
            for b in 0..NBUF {
                if x.pos >= x.size {
                    break;
                }
                let off = x.pos;
                x.pos += CHUNK as u64;
                x.submit(&self.disk, b, 0, off);
            }
        }
    }

    fn finish_data(&mut self, i: usize, mut ok: bool, msg: &str) {
        let mut msg = msg.to_string();
        let s = &mut self.sess[i];
        let upload = s.mode == Mode::Stor;
        let size = s.x.pos;
        if s.x.file.is_some() || s.x.untar.is_some() {
            s.xfer_close();
            if s.x.err {
                ok = false;
                msg = "451 SD card error".into();
            }
        }
        let mut unpacked = Vec::new();
        let bundle = s.x.untar.take();
        if let Some(u) = &bundle {
            let mut u = u.lock().unwrap();
            if ok && u.clean_end() {
                msg = if u.errors == 0 {
                    format!("226 Unpacked {} files", u.files)
                } else {
                    format!("451 Unpacked with {} error(s), first: {}", u.errors, u.first_error)
                };
            } else {
                u.abort(); // cut short: the half written file goes, the client sends again
                ok = false;
                msg = "426 Connection lost".into();
            }
            unpacked = std::mem::take(&mut u.cias);
            if s.batch_files != 0 {
                s.batch_files_done = s.bundle_base + u.files;
            }
        }
        if (s.mode == Mode::Stor || s.mode == Mode::Retr) && s.batch_files != 0 {
            s.batch_bytes_done += s.x.done;
            if ok && bundle.is_none() {
                s.batch_files_done += 1;
            }
        }
        let path = s.path.clone();
        if upload && ok && bundle.is_none() {
            self.cia_add(&path, size);
        }
        for (cia, size) in unpacked {
            self.cia_add(&cia, size);
        }
        self.last_activity = sys::now_ms();
        let s = &mut self.sess[i];
        if ok {
            let mut data = s.data.take();
            self.net.close_graceful(&mut data);
        } else {
            close_now(&mut s.data);
        }
        let s = &mut self.sess[i];
        s.end_transfer();
        s.reply(&format!("{msg}\r\n"));
    }

    /// network -> SD
    fn pump_stor(&mut self, i: usize) {
        loop {
            let s = &mut self.sess[i];
            let Some(data) = s.data else { return };
            if let Some(u) = &s.x.untar {
                // a bundle: the entry being unpacked and the files done, if the disk thread lets go
                if let Ok(u) = u.try_lock() {
                    if !u.current.is_empty() {
                        s.name = util::clip(util::base_name(&u.current), 63).to_string();
                    }
                    if s.batch_files != 0 {
                        s.batch_files_done = s.bundle_base + u.files;
                    }
                }
            }
            let x = &mut s.x;
            x.collect();
            let c = x.cur;
            match x.slots[c] {
                Slot::Busy => return, // all buffers are being written: wait for the SD card
                Slot::Done { ok: false, .. } => {
                    self.finish_data(i, false, "451 SD card write error");
                    return;
                }
                _ => {}
            }
            x.slots[c] = match std::mem::replace(&mut x.slots[c], Slot::Empty) {
                Slot::Done { buf, .. } | Slot::Free(buf) => Slot::Fill { buf, len: 0, off: x.pos },
                other => other,
            };
            let Slot::Fill { buf, len, off } = &mut x.slots[c] else { return };
            match net::recv(data, &mut buf[*len..CHUNK]) {
                Io::Done(n) => {
                    *len += n;
                    x.pos += n as u64;
                    x.done += n as u64;
                    if *len == CHUNK {
                        let off = *off;
                        x.submit(&self.disk, c, CHUNK, off);
                        x.cur = (c + 1) % NBUF;
                    }
                }
                Io::WouldBlock => return,
                end => {
                    // closed: upload complete; failed: connection lost
                    let (len, off) = (*len, *off);
                    if len > 0 {
                        x.submit(&self.disk, c, len, off);
                    } else if let Slot::Fill { buf, .. } = std::mem::replace(&mut x.slots[c], Slot::Empty) {
                        x.slots[c] = Slot::Free(buf);
                    }
                    if matches!(end, Io::Closed) {
                        self.finish_data(i, true, "226 Transfer complete");
                    } else {
                        self.finish_data(i, false, "426 Connection lost");
                    }
                    return;
                }
            }
        }
    }

    /// SD -> network
    fn pump_retr(&mut self, i: usize) {
        loop {
            let s = &mut self.sess[i];
            let Some(data) = s.data else { return };
            let x = &mut s.x;
            x.collect();
            let c = x.cur;
            let (len, ok) = match &x.slots[c] {
                Slot::Busy => return, // waiting for the SD card
                Slot::Done { len, ok, .. } => (*len, *ok),
                _ => {
                    self.finish_data(i, true, "226 Transfer complete");
                    return;
                }
            };
            if !ok {
                self.finish_data(i, false, "451 SD card read error");
                return;
            }
            if len == 0 {
                self.finish_data(i, true, "226 Transfer complete");
                return;
            }
            let Slot::Done { buf, .. } = &x.slots[c] else { return };
            match net::send(data, &buf[x.send_pos..len]) {
                Io::Done(n) => {
                    x.send_pos += n;
                    x.done += n as u64;
                }
                Io::WouldBlock => return,
                _ => {
                    self.finish_data(i, false, "426 Connection lost");
                    return;
                }
            }
            if x.send_pos < len {
                continue;
            }
            // buffer fully sent: refill it with the next chunk
            x.send_pos = 0;
            x.cur = (c + 1) % NBUF;
            if x.pos < x.size {
                let off = x.pos;
                x.pos += CHUNK as u64;
                x.submit(&self.disk, c, 0, off);
            } else if let Slot::Done { buf, .. } = std::mem::replace(&mut x.slots[c], Slot::Empty) {
                x.slots[c] = Slot::Free(buf);
            }
        }
    }

    fn pump_list(&mut self, i: usize) {
        let s = &mut self.sess[i];
        let Some(data) = s.data else { return };
        while s.list_pos < s.list.len() {
            match net::send(data, &s.list[s.list_pos..]) {
                Io::Done(n) => s.list_pos += n,
                Io::WouldBlock => return,
                _ => {
                    self.finish_data(i, false, "426 Connection lost");
                    return;
                }
            }
        }
        self.finish_data(i, true, "226 Done");
    }

    // commands

    fn handle_command(&mut self, i: usize, line: &[u8]) {
        let (word, arg) = match line.iter().position(|&c| c == b' ') {
            Some(p) => (&line[..p], &line[p + 1..]),
            None => (line, &line[line.len()..]),
        };
        let cmd = String::from_utf8_lossy(word).to_ascii_uppercase();
        let cmd = cmd.as_str();
        let now = sys::now_ms();
        self.sess[i].last_cmd = now;
        // before anything below changes what the 3DS does
        if !self.logged_in(i) && !matches!(cmd, "USER" | "PASS" | "QUIT" | "FEAT" | "SYST" | "NOOP" | "OPTS") {
            self.sess[i].reply("530 Please log in with USER and PASS\r\n");
            return;
        }
        // a client that only polls (Baguette's status) isn't someone using the 3DS
        let poll = cmd == "NOOP" || (cmd == "SITE" && arg.len() >= 6 && arg[..6].eq_ignore_ascii_case(b"STATUS"));
        if !poll && self.logged_in(i) {
            self.last_ftp = now;
        }

        // a deletion is a stream of DELE/RMD commands: count it as busy for a moment
        if matches!(cmd, "DELE" | "RMD" | "XRMD" | "RNFR" | "RNTO" | "MKD" | "XMKD") {
            self.fs_busy_until = now + 2000;
            self.last_activity = now + 2000;
        }
        {
            let s = &mut self.sess[i];
            if matches!(cmd, "DELE" | "RMD" | "XRMD")
                && s.batch_files != 0
                && s.batch_kind == BatchKind::Delete
                && s.batch_files_done < s.batch_files
            {
                s.batch_files_done += 1;
            }
        }
        if matches!(cmd, "RETR" | "APPE" | "LIST" | "NLST" | "MLSD") {
            self.sess[i].bundle = None; // only the very next STOR is a bundle
        }

        let busy = self.sess[i].mode != Mode::None;
        if busy && matches!(cmd, "PASV" | "EPSV" | "LIST" | "NLST" | "MLSD" | "RETR" | "STOR" | "APPE") {
            self.sess[i].reply("450 Transfer in progress\r\n");
            self.sess[i].rest = 0;
            return;
        }
        match cmd {
            "USER" | "PASS" => self.login_command(i, cmd, arg, now),
            "PWD" | "XPWD" | "CWD" | "CDUP" | "XCUP" | "MLST" | "SIZE" | "MDTM" | "AVBL" => self.navigation_command(i, cmd, arg),
            "PASV" | "EPSV" | "PORT" | "EPRT" | "LIST" | "NLST" | "MLSD" | "RETR" | "STOR" | "APPE" | "ABOR" => self.transfer_command(i, cmd, arg),
            "DELE" | "RMD" | "XRMD" | "MKD" | "XMKD" | "RNFR" | "RNTO" => self.file_command(i, cmd, arg),
            "SYST" => self.sess[i].reply("215 UNIX Type: L8\r\n"),
            "FEAT" => self.sess[i].reply(
                "211-Features:\r\n MLST type*;size*;\r\n MLSD\r\n SIZE\r\n MDTM\r\n AVBL\r\n REST STREAM\r\n UTF8\r\n EPSV\r\n211 End\r\n",
            ),
            "OPTS" | "TYPE" | "MODE" | "STRU" | "NOOP" => self.sess[i].reply("200 OK\r\n"),
            "ALLO" => {
                let s = &mut self.sess[i];
                s.allo = util::parse_ull(arg); // size of the next STOR, for progress/ETA
                s.reply("200 OK\r\n");
            }
            "SITE" => self.site(i, arg),
            "REST" => {
                let s = &mut self.sess[i];
                s.rest = util::parse_ull(arg);
                let text = format!("350 Restarting at {}\r\n", s.rest);
                s.reply(&text);
                return; // keep rest for the next command
            }
            "QUIT" => {
                let s = &mut self.sess[i];
                s.reply("221 Bye\r\n");
                s.close();
            }
            _ => self.sess[i].reply("502 Not implemented\r\n"),
        }
        self.sess[i].rest = 0;
    }

    /// USER and PASS
    fn login_command(&mut self, i: usize, cmd: &str, arg: &[u8], now: u64) {
        match cmd {
            "USER" => {
                let user = String::from_utf8_lossy(arg).into_owned();
                let s = &mut self.sess[i];
                if self.login.is_none() {
                    s.logged_in = true; // no login set: anyone, any name
                    s.reply("230 Logged in\r\n");
                } else {
                    s.logged_in = false;
                    s.user = user;
                    s.reply("331 Password required\r\n");
                }
            }
            "PASS" => {
                let pass = String::from_utf8_lossy(arg).into_owned();
                let s = &mut self.sess[i];
                // a check takes about 50 ms of the main loop: after a wrong password
                // nobody gets another one checked for a second, so guessing can't
                // stall the 3DS, and a connection gets three tries
                let waiting = now < self.pass_after;
                match &self.login {
                    None => {
                        s.logged_in = true;
                        s.reply("230 Logged in\r\n");
                    }
                    Some((user, hash)) if !waiting && s.user == *user && settings::check_pass(hash, &pass) => {
                        s.logged_in = true;
                        s.wrong = 0;
                        s.reply("230 Logged in\r\n");
                    }
                    Some(_) => {
                        s.logged_in = false;
                        s.user.clear();
                        s.wrong += 1;
                        if !waiting {
                            self.pass_after = now + WRONG_PASS_MS;
                        }
                        if s.wrong >= MAX_WRONG_PASS {
                            s.reply("421 Too many wrong passwords\r\n");
                            s.close();
                        } else if waiting {
                            s.reply("530 Wait a second and try again\r\n");
                        } else {
                            s.reply("530 Login incorrect\r\n");
                        }
                    }
                }
            }
            _ => self.sess[i].reply("502 Not implemented\r\n"),
        }
    }

    /// the working folder and what is in it, without the data connection
    fn navigation_command(&mut self, i: usize, cmd: &str, arg: &[u8]) {
        let cwd = self.sess[i].cwd.clone();
        let resolve = |a: &[u8]| resolve_path(&cwd, a);
        match cmd {
            "PWD" | "XPWD" => {
                let s = &mut self.sess[i];
                let text = format!("257 \"{}\"\r\n", s.cwd);
                s.reply(&text);
            }
            "CWD" | "CDUP" | "XCUP" => {
                let target = if cmd == "CWD" { resolve(arg) } else { resolve(b"..") };
                let s = &mut self.sess[i];
                match target {
                    Some(p) if sys::is_dir(&p) => {
                        s.cwd = p;
                        s.reply("250 OK\r\n");
                    }
                    _ => s.reply("550 No such directory\r\n"),
                }
            }
            "MLST" => {
                let text = match resolve(arg) {
                    None => "550 Bad path\r\n".to_string(),
                    Some(p) if sys::is_dir(&p) => format!("250-Listing\r\n type=dir;size=0; {p}\r\n250 End\r\n"),
                    Some(p) => match sys::file_size(&p) {
                        Some(size) => format!("250-Listing\r\n type=file;size={size}; {p}\r\n250 End\r\n"),
                        None => "550 Not found\r\n".to_string(),
                    },
                };
                self.sess[i].reply(&text);
            }
            "SIZE" => {
                let text = match resolve(arg).and_then(|p| sys::file_size(&p)) {
                    Some(size) => format!("213 {size}\r\n"),
                    None => "550 Not found\r\n".to_string(),
                };
                self.sess[i].reply(&text);
            }
            "MDTM" => {
                let stamp = resolve(arg).filter(|p| sys::file_size(p).is_some()).and_then(|p| sys::mtime(&p));
                let text = match stamp {
                    Some(ms) => format!("213 {}\r\n", util::mdtm_text(ms)),
                    None => "550 Not found\r\n".to_string(),
                };
                self.sess[i].reply(&text);
            }
            "AVBL" => {
                // free space; the whole card is one volume, so the path doesn't matter
                let text = match sys::sd_space() {
                    Some((free, _)) => format!("213 {free}\r\n"),
                    None => "550 Can't read free space\r\n".to_string(),
                };
                self.sess[i].reply(&text);
            }
            _ => self.sess[i].reply("502 Not implemented\r\n"),
        }
    }

    /// the data connection and what goes over it
    fn transfer_command(&mut self, i: usize, cmd: &str, arg: &[u8]) {
        let cwd = self.sess[i].cwd.clone();
        let resolve = |a: &[u8]| resolve_path(&cwd, a);
        match cmd {
            "PASV" => self.open_pasv(i, false),
            "EPSV" => self.open_pasv(i, true),
            "PORT" | "EPRT" => self.sess[i].reply("502 Use PASV\r\n"),
            "LIST" | "NLST" | "MLSD" => {
                let mode = match cmd {
                    "LIST" => Mode::List,
                    "NLST" => Mode::Nlst,
                    _ => Mode::Mlsd,
                };
                let mut a = arg;
                while a.first() == Some(&b'-') {
                    // skip "-a", "-la" ...
                    let end = a.iter().position(|&c| c == b' ').unwrap_or(a.len());
                    a = &a[end..];
                    while a.first() == Some(&b' ') {
                        a = &a[1..];
                    }
                }
                let path = resolve(a);
                let s = &mut self.sess[i];
                if s.pasv.is_none() && s.data.is_none() {
                    s.reply("425 Use PASV first\r\n");
                } else {
                    let mut entries = Vec::new();
                    let listed = path.is_some_and(|p| {
                        sys::list_dir(&p, &mut |name, dir, size| entries.push((name.to_string(), dir, size)))
                    });
                    if !listed {
                        s.end_transfer();
                        s.reply("550 No such directory\r\n");
                    } else {
                        for (name, dir, size) in entries {
                            s.list_entry(mode, &name, dir, size);
                        }
                        s.mode = mode;
                        s.reply("150 Listing\r\n");
                    }
                }
            }
            "RETR" | "STOR" | "APPE" => {
                let mode = if cmd == "RETR" { Mode::Retr } else { Mode::Stor };
                let path = resolve(arg);
                let low = self.battery_refusal();
                let s = &mut self.sess[i];
                if s.pasv.is_none() && s.data.is_none() {
                    s.reply("425 Use PASV first\r\n");
                } else if let (Some(dir), "STOR") = (s.bundle.take(), cmd) {
                    // SITE BUNDLE: the name is ignored, the stream is unpacked into dir
                    if let Some(text) = low {
                        s.end_transfer();
                        s.reply(&text);
                    } else if self.prepare_bundle(i, &dir) {
                        let s = &mut self.sess[i];
                        s.mode = Mode::Stor;
                        s.set_name(&dir);
                        s.reply("150 Opening data connection\r\n");
                    }
                } else if let Some(path) = path {
                    if let Some(text) = low {
                        s.end_transfer();
                        s.reply(&text);
                    } else if self.prepare_file(i, &path, mode, cmd == "APPE") {
                        let s = &mut self.sess[i];
                        s.mode = mode;
                        s.set_name(&path);
                        s.reply("150 Opening data connection\r\n");
                    }
                } else {
                    s.reply("553 Bad file name\r\n");
                }
            }
            "ABOR" => {
                let s = &mut self.sess[i];
                if s.mode != Mode::None {
                    close_now(&mut s.data);
                    s.end_transfer();
                    s.reply("426 Aborted\r\n");
                }
                s.reply("226 Abort OK\r\n");
            }
            _ => self.sess[i].reply("502 Not implemented\r\n"),
        }
    }

    /// deleting, creating and renaming on the SD card
    fn file_command(&mut self, i: usize, cmd: &str, arg: &[u8]) {
        let cwd = self.sess[i].cwd.clone();
        let resolve = |a: &[u8]| resolve_path(&cwd, a);
        match cmd {
            "DELE" => match resolve(arg) {
                Some(p) if sys::delete_file(&p) => {
                    self.cia_remove(&p);
                    self.sess[i].reply("250 Deleted\r\n");
                }
                _ => self.sess[i].reply("550 Delete failed\r\n"),
            },
            "RMD" | "XRMD" => match resolve(arg) {
                // recursive on purpose: removing a folder in one command is much faster
                Some(p) if p != "/" && sys::delete_dir_recursive(&p) => {
                    self.cia_remove(&p);
                    self.sess[i].reply("250 Removed\r\n");
                }
                _ => self.sess[i].reply("550 Remove failed\r\n"),
            },
            "MKD" | "XMKD" => match resolve(arg) {
                Some(p) if sys::create_dir(&p) => self.sess[i].reply(&format!("257 \"{p}\" created\r\n")),
                _ => self.sess[i].reply("550 Create failed\r\n"),
            },
            "RNFR" => {
                let s = &mut self.sess[i];
                match resolve(arg) {
                    Some(p) if sys::is_dir(&p) || sys::file_size(&p).is_some() => {
                        s.rnfr = p;
                        s.reply("350 Ready\r\n");
                    }
                    _ => {
                        s.rnfr.clear();
                        s.reply("550 Not found\r\n");
                    }
                }
            }
            "RNTO" => {
                let from = std::mem::take(&mut self.sess[i].rnfr);
                match resolve(arg) {
                    Some(to) if !from.is_empty() && sys::rename(&from, &to) => {
                        self.cia_remove(&from);
                        if let Some(size) = sys::file_size(&to) {
                            self.cia_add(&to, size);
                        }
                        self.sess[i].reply("250 Renamed\r\n");
                    }
                    _ => self.sess[i].reply("550 Rename failed\r\n"),
                }
            }
            _ => self.sess[i].reply("502 Not implemented\r\n"),
        }
    }

    fn site(&mut self, i: usize, arg: &[u8]) {
        let text = String::from_utf8_lossy(arg);
        let sub: String =
            text.split_whitespace().next().unwrap_or("").chars().take(15).collect::<String>().to_ascii_uppercase();
        match sub.as_str() {
            "STATUS" => {
                let text = self.site_status(i);
                self.sess[i].reply(&text);
            }
            "BATCH" => self.site_batch(i, &text),
            "COLOR" => self.site_color(i, &text),
            "INSTALL" => {
                // SITE INSTALL <path>: as pressing A on that CIA, except that system titles
                // are refused and a reinstall or downgrade waits for A on the console
                let arg = text.get(7..).unwrap_or("").trim_start();
                let start = match util::resolve_path(&self.sess[i].cwd, arg.as_bytes()) {
                    Some(path) if !arg.is_empty() => self.request_install(&path, true),
                    _ => InstallStart::Refused(550, "No such CIA"),
                };
                let reply = match start {
                    InstallStart::Started => "200 Install started\r\n".to_string(),
                    InstallStart::Queued => "200 Install queued\r\n".to_string(),
                    InstallStart::Asked => "200 Press A on the 3DS to confirm\r\n".to_string(),
                    InstallStart::Refused(code, why) => format!("{code} {why}\r\n"),
                };
                self.sess[i].reply(&reply);
            }
            "LOGIN" => {
                // SITE LOGIN <user> <password>: the password is the rest of the line;
                // SITE LOGIN alone: back to anonymous
                let rest = text.get(5..).unwrap_or("").trim_start();
                let reply = if rest.is_empty() {
                    self.set_login(None);
                    "200 Login cleared\r\n"
                } else {
                    let (user, pass) = rest.split_once(' ').unwrap_or((rest, ""));
                    if settings::valid_user(user) {
                        self.set_login(Some((user.to_string(), pass.to_string())));
                        "200 Login set\r\n"
                    } else {
                        "501 SITE LOGIN <user> <password>\r\n"
                    }
                };
                self.sess[i].logged_in = true; // whoever set it stays in
                self.sess[i].reply(reply);
            }
            "BUNDLE" => {
                // SITE BUNDLE <dir>: the next STOR is a plain tar unpacked into dir
                let arg = text.get(6..).unwrap_or("").trim();
                let s = &mut self.sess[i];
                match resolve_path(&s.cwd, arg.as_bytes()) {
                    Some(dir) if !arg.is_empty() => {
                        s.bundle = Some(dir);
                        s.reply("200 Send the bundle\r\n");
                    }
                    _ => s.reply("501 SITE BUNDLE <dir>\r\n"),
                }
            }
            _ => self.sess[i].reply("502 SITE STATUS, BATCH, COLOR, INSTALL, LOGIN or BUNDLE\r\n"),
        }
    }

    /// SITE BATCH <files> <bytes> [up|down|delete]; "SITE BATCH 0 0" ends it
    fn site_batch(&mut self, i: usize, text: &str) {
        let rest = text.get(5..).unwrap_or("");
        let mut tok = rest.split_whitespace();
        fn num(t: Option<&str>) -> Option<&str> {
            t.filter(|t| t.starts_with(|c: char| c.is_ascii_digit() || c == '+'))
        }
        let (mut files, mut bytes, mut kind) = (0u64, 0u64, BatchKind::Up);
        if let Some(t) = num(tok.next()) {
            files = util::parse_ull(t.as_bytes());
            if let Some(t) = num(tok.next()) {
                bytes = util::parse_ull(t.as_bytes());
                if let Some(t) = tok.next() {
                    kind = BatchKind::parse(t);
                }
            }
        }
        let now = sys::now_ms();
        let s = &mut self.sess[i];
        s.batch_files = files as u32;
        s.batch_bytes = bytes;
        s.batch_files_done = 0;
        s.batch_bytes_done = 0;
        s.batch_start = now;
        s.batch_kind = kind;
        self.last_activity = now;
        self.sess[i].reply("200 OK\r\n");
    }

    /// SITE COLOR [#rrggbb [secs]]: the red look's colour and when it was
    /// picked (local clock, now when not given), kept on the SD card.
    /// Baguette compares the times and sends the newer colour, so it's
    /// always taken
    fn site_color(&mut self, i: usize, text: &str) {
        let mut args = text.get(5..).unwrap_or("").split_whitespace();
        let (c, t) = (args.next(), args.next());
        let color = c.and_then(settings::parse_color);
        let time = t.map(|t| t.parse::<u64>().ok());
        match (c, color, time, args.next()) {
            (None, ..) => {
                let text = format!("200 #{:06x} {}\r\n", self.accent, self.accent_time);
                self.sess[i].reply(&text);
            }
            (Some(_), Some(color), None | Some(Some(_)), None) => {
                let time = time.flatten().unwrap_or_else(|| sys::unix_time().unwrap_or(0));
                self.set_accent(color, time);
                self.sess[i].reply("200 Accent set\r\n");
            }
            _ => self.sess[i].reply("501 SITE COLOR #rrggbb [secs]\r\n"),
        }
    }

    /// SITE STATUS: what the other connections are doing, for scripts and tools
    fn site_status(&mut self, me: usize) -> String {
        let now = sys::now_ms();
        let mut busy = self.installing() || now < self.fs_busy_until;
        let mut clients = 0;
        for (j, s) in self.sess.iter().enumerate() {
            if s.ctrl.is_none() {
                continue;
            }
            clients += 1;
            if j != me && s.working() {
                busy = true;
            }
        }
        let (sd_free, sd_total) = sys::sd_space().unwrap_or((0, 0));
        let bat = self.battery();
        let idle = if busy || now < self.last_activity { 0 } else { now - self.last_activity };
        let mut out = String::from("211-Status\r\n");
        out += &format!(
            " busy={} idle_ms={} installing={} connections={} protocol={} sd_free={} sd_total={} battery={} battery_bars={} charging={} plugged={} battery_low={} accent={:06x} accent_time={} login={} color_sync={} version={}\r\n",
            busy as i32,
            idle,
            self.installing() as i32,
            clients,
            PROTOCOL,
            sd_free,
            sd_total,
            bat.percent,
            bat.bars,
            bat.charging as i32,
            bat.plugged as i32,
            crate::battery_low(bat) as i32,
            self.accent,
            self.accent_time,
            self.login.is_some() as i32,
            1, // color_sync: the colour syncs both ways, the newer accent_time wins
            VERSION
        );
        for (j, s) in self.sess.iter().enumerate() {
            if j == me || !s.working() {
                continue;
            }
            let p = progress_of(s, now);
            let batch = if s.batch_files != 0 {
                format!(
                    " batch={} batch_files_done={} batch_files={} batch_bytes_done={} batch_bytes={} batch_eta={}",
                    s.batch_kind.name(),
                    s.batch_files_done,
                    s.batch_files,
                    p.batch_done,
                    p.batch_bytes,
                    p.batch_eta
                )
            } else {
                String::new()
            };
            out += &format!(
                " transfer peer={} dir={} done={} size={} bps={} eta={}{} file={}\r\n",
                util::ip_text(s.peer),
                if !p.active {
                    "none"
                } else if s.mode == Mode::Stor {
                    "up"
                } else {
                    "down"
                },
                p.done,
                p.size,
                p.bps,
                p.eta,
                batch,
                if p.active { s.path.as_str() } else { "" }
            );
        }
        if let Some(inst) = &self.inst {
            out += &format!(
                " install done={} size={} file={}\r\n",
                *inst.shared.done.lock().unwrap(),
                inst.total,
                inst.path
            );
        }
        for c in &self.cias {
            out += &format!(" cia size={} file={}\r\n", c.size, c.path);
        }
        if let Some(r) = &self.install_result {
            let code = if r.ok { "0".to_string() } else { format!("{:08x}", r.code) };
            out += &format!(" install_result n={} ok={} code={} file={}\r\n", r.n, r.ok as i32, code, r.path);
            if !r.ok {
                out += &format!(" install_error {}\r\n", r.msg);
            }
        }
        out += "211 End\r\n";
        out
    }

    /// with no login set, every session is in
    fn logged_in(&self, i: usize) -> bool {
        self.login.is_none() || self.sess[i].logged_in
    }

    /// sets (user, password) or clears the FTP login and keeps it on the SD
    /// card, the password as a hash
    pub fn set_login(&mut self, login: Option<(String, String)>) {
        let login = login.map(|(u, p)| (u, settings::hash_pass(&p)));
        settings::save_login(login.as_ref().map(|(u, h)| (u.as_str(), h.as_str())));
        self.login = login;
        self.last_time = 0; // the top screen shows it
        self.bottom_dirty = true;
    }

    /// SITE BUNDLE's STOR: the received bytes go to an unpacker on the disk thread
    fn prepare_bundle(&mut self, i: usize, dir: &str) -> bool {
        if !self.sess[i].x.alloc() {
            self.sess[i].reply("451 Out of memory\r\n");
            return false;
        }
        let s = &mut self.sess[i];
        let x = &mut s.x;
        x.reset(true);
        x.file = None;
        x.untar = Some(Arc::new(Mutex::new(Untar::new(dir))));
        x.pos = 0;
        s.xfer_base = 0;
        s.xfer_total = s.allo;
        s.allo = 0;
        s.bundle_base = s.batch_files_done;
        true
    }

    /// the reply refusing a transfer while the battery is low
    fn battery_refusal(&mut self) -> Option<String> {
        let bat = self.battery();
        crate::battery_low(bat)
            .then(|| format!("450 3DS battery too low ({}): plug in the charger\r\n", crate::battery_text(bat)))
    }

    /// installs wait for transfers and deletions: they share the SD card
    pub fn fs_busy(&self) -> bool {
        sys::now_ms() < self.fs_busy_until || self.sess.iter().any(|s| s.working())
    }
}
