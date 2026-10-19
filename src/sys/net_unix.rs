// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// BSD sockets through libc: the 3DS (libctru's soc service) and Linux.

use std::mem::size_of;

pub type Sock = libc::c_int;

pub enum Io {
    Done(usize), // > 0 bytes
    Closed,      // the peer closed (recv)
    WouldBlock,
    Failed,
}

pub const POLL_IN: i16 = libc::POLLIN as i16;
pub const POLL_OUT: i16 = libc::POLLOUT as i16;

#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: Sock,
    pub events: i16,
    pub revents: i16,
}

#[cfg_attr(target_os = "horizon", allow(dead_code))] // Winsock needs it, BSD sockets don't
pub fn startup() {}

fn would_block() -> bool {
    let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    e == libc::EAGAIN || e == libc::EWOULDBLOCK
}

fn set_int(fd: Sock, level: libc::c_int, name: libc::c_int, value: libc::c_int) {
    unsafe {
        libc::setsockopt(fd, level, name, (&raw const value).cast(), size_of::<libc::c_int>() as libc::socklen_t);
    }
}

pub fn tcp_socket() -> Option<Sock> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    (fd >= 0).then_some(fd)
}

pub fn set_nonblocking(fd: Sock) {
    unsafe {
        libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL, 0) | libc::O_NONBLOCK);
    }
}

pub fn set_buffers(fd: Sock, size: i32) {
    set_int(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, size);
    set_int(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, size);
}

pub fn set_reuse_addr(fd: Sock) {
    set_int(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
}

/// replies go out at once: Nagle held "226" until the client's delayed ACK of
/// "150" arrived, about 200 ms per uploaded file
pub fn set_nodelay(fd: Sock) {
    set_int(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, 1);
}

fn addr(ip: u32, port: u16) -> libc::sockaddr_in {
    let mut a: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    a.sin_family = libc::AF_INET as libc::sa_family_t;
    a.sin_port = port.to_be();
    a.sin_addr.s_addr = ip;
    a
}

/// ip: s_addr (network order)
pub fn bind(fd: Sock, ip: u32, port: u16) -> bool {
    let a = addr(ip, port);
    unsafe { libc::bind(fd, (&raw const a).cast(), size_of::<libc::sockaddr_in>() as libc::socklen_t) == 0 }
}

pub fn listen(fd: Sock, backlog: i32) -> bool {
    unsafe { libc::listen(fd, backlog) == 0 }
}

/// the new socket and the peer's s_addr
pub fn accept(fd: Sock) -> Option<(Sock, u32)> {
    let mut a: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let s = unsafe { libc::accept(fd, (&raw mut a).cast(), &mut len) };
    (s >= 0).then_some((s, a.sin_addr.s_addr))
}

pub fn recv(fd: Sock, buf: &mut [u8]) -> Io {
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    match n {
        0 => Io::Closed,
        n if n > 0 => Io::Done(n as usize),
        _ if would_block() => Io::WouldBlock,
        _ => Io::Failed,
    }
}

pub fn send(fd: Sock, buf: &[u8]) -> Io {
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_NOSIGNAL;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    let n = unsafe { libc::send(fd, buf.as_ptr().cast(), buf.len(), flags) };
    match n {
        n if n > 0 => Io::Done(n as usize),
        0 => Io::Failed,
        _ if would_block() => Io::WouldBlock,
        _ => Io::Failed,
    }
}

/// half-close, then close at once (no lingering) when we close it later
pub fn shutdown_write(fd: Sock) {
    unsafe {
        libc::shutdown(fd, libc::SHUT_WR);
        let l = libc::linger { l_onoff: 1, l_linger: 0 };
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_LINGER, (&raw const l).cast(), size_of::<libc::linger>() as libc::socklen_t);
    }
}

pub fn close(fd: Sock) {
    unsafe {
        libc::close(fd);
    }
}

/// < 0: the network went away
pub fn poll(fds: &mut [PollFd], timeout_ms: i32) -> i32 {
    let mut raw: Vec<libc::pollfd> =
        fds.iter().map(|p| libc::pollfd { fd: p.fd, events: p.events as _, revents: 0 }).collect();
    let rc = unsafe { libc::poll(raw.as_mut_ptr(), raw.len() as libc::nfds_t, timeout_ms) };
    for (p, r) in fds.iter_mut().zip(&raw) {
        p.revents = r.revents as i16;
    }
    rc
}
