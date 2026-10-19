// Winsock, for the Windows test build only (the GUI client's engine test runs a server here).

use std::mem::size_of;

use windows_sys::Win32::Networking::WinSock as ws;

pub type Sock = ws::SOCKET;

pub enum Io {
    Done(usize),
    Closed,
    WouldBlock,
    Failed,
}

pub const POLL_IN: i16 = ws::POLLRDNORM as i16;
pub const POLL_OUT: i16 = ws::POLLWRNORM as i16;

#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: Sock,
    pub events: i16,
    pub revents: i16,
}

pub fn startup() {
    unsafe {
        let mut data: ws::WSADATA = std::mem::zeroed();
        ws::WSAStartup(0x0202, &mut data);
    }
}

fn would_block() -> bool {
    unsafe { ws::WSAGetLastError() == ws::WSAEWOULDBLOCK }
}

fn set_int(fd: Sock, level: i32, name: i32, value: i32) {
    unsafe {
        ws::setsockopt(fd, level, name, (&raw const value).cast(), size_of::<i32>() as i32);
    }
}

pub fn tcp_socket() -> Option<Sock> {
    let fd = unsafe { ws::socket(ws::AF_INET as i32, ws::SOCK_STREAM, 0) };
    (fd != ws::INVALID_SOCKET).then_some(fd)
}

pub fn set_nonblocking(fd: Sock) {
    let mut on = 1u32;
    unsafe {
        ws::ioctlsocket(fd, ws::FIONBIO, &mut on);
    }
}

pub fn set_buffers(fd: Sock, size: i32) {
    set_int(fd, ws::SOL_SOCKET, ws::SO_RCVBUF, size);
    set_int(fd, ws::SOL_SOCKET, ws::SO_SNDBUF, size);
}

pub fn set_reuse_addr(fd: Sock) {
    set_int(fd, ws::SOL_SOCKET, ws::SO_REUSEADDR, 1);
}

pub fn set_nodelay(fd: Sock) {
    set_int(fd, ws::IPPROTO_TCP, ws::TCP_NODELAY, 1);
}

fn addr(ip: u32, port: u16) -> ws::SOCKADDR_IN {
    let mut a: ws::SOCKADDR_IN = unsafe { std::mem::zeroed() };
    a.sin_family = ws::AF_INET;
    a.sin_port = port.to_be();
    a.sin_addr.S_un.S_addr = ip;
    a
}

pub fn bind(fd: Sock, ip: u32, port: u16) -> bool {
    let a = addr(ip, port);
    unsafe { ws::bind(fd, (&raw const a).cast(), size_of::<ws::SOCKADDR_IN>() as i32) == 0 }
}

pub fn listen(fd: Sock, backlog: i32) -> bool {
    unsafe { ws::listen(fd, backlog) == 0 }
}

pub fn accept(fd: Sock) -> Option<(Sock, u32)> {
    let mut a: ws::SOCKADDR_IN = unsafe { std::mem::zeroed() };
    let mut len = size_of::<ws::SOCKADDR_IN>() as i32;
    let s = unsafe { ws::accept(fd, (&raw mut a).cast(), &mut len) };
    (s != ws::INVALID_SOCKET).then(|| (s, unsafe { a.sin_addr.S_un.S_addr }))
}

pub fn recv(fd: Sock, buf: &mut [u8]) -> Io {
    let n = unsafe { ws::recv(fd, buf.as_mut_ptr(), buf.len().min(i32::MAX as usize) as i32, 0) };
    match n {
        0 => Io::Closed,
        n if n > 0 => Io::Done(n as usize),
        _ if would_block() => Io::WouldBlock,
        _ => Io::Failed,
    }
}

pub fn send(fd: Sock, buf: &[u8]) -> Io {
    let n = unsafe { ws::send(fd, buf.as_ptr(), buf.len().min(i32::MAX as usize) as i32, 0) };
    match n {
        n if n > 0 => Io::Done(n as usize),
        0 => Io::Failed,
        _ if would_block() => Io::WouldBlock,
        _ => Io::Failed,
    }
}

pub fn shutdown_write(fd: Sock) {
    unsafe {
        ws::shutdown(fd, ws::SD_SEND);
        let l = ws::LINGER { l_onoff: 1, l_linger: 0 };
        ws::setsockopt(fd, ws::SOL_SOCKET, ws::SO_LINGER, (&raw const l).cast(), size_of::<ws::LINGER>() as i32);
    }
}

pub fn close(fd: Sock) {
    unsafe {
        ws::closesocket(fd);
    }
}

pub fn poll(fds: &mut [PollFd], timeout_ms: i32) -> i32 {
    let mut raw: Vec<ws::WSAPOLLFD> =
        fds.iter().map(|p| ws::WSAPOLLFD { fd: p.fd, events: p.events, revents: 0 }).collect();
    let rc = unsafe { ws::WSAPoll(raw.as_mut_ptr(), raw.len() as u32, timeout_ms) };
    for (p, r) in fds.iter_mut().zip(&raw) {
        p.revents = r.revents;
    }
    rc
}
