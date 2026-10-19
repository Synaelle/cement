// The disk thread: every SD card read and write of a transfer happens here, so
// receiving from the network and writing to the SD card overlap. A transfer
// owns NBUF buffers; a buffer is handed to the disk thread by value and comes
// back on the transfer's own channel once it has been written or read.

use std::alloc::{Layout, alloc, dealloc};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::sys;

pub const NBUF: usize = 4;
pub const CHUNK: usize = 256 * 1024;

/// CHUNK bytes, 64-byte aligned (cache lines, like the C server's memalign)
pub struct Buf(*mut u8);

unsafe impl Send for Buf {}

impl Buf {
    const LAYOUT: Layout = match Layout::from_size_align(CHUNK, 64) {
        Ok(l) => l,
        Err(_) => panic!(),
    };

    pub fn new() -> Option<Buf> {
        let p = unsafe { alloc(Self::LAYOUT) };
        (!p.is_null()).then_some(Buf(p))
    }
}

impl std::ops::Deref for Buf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.0, CHUNK) }
    }
}

impl std::ops::DerefMut for Buf {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.0, CHUNK) }
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        unsafe { dealloc(self.0, Self::LAYOUT) }
    }
}

/// a finished disk job, sent back to the transfer
pub struct Done {
    pub slot: usize,
    pub buf: Buf,
    pub len: usize, // bytes written, or read (0 at the end of the file)
    pub ok: bool,
}

pub struct Job {
    pub file: Arc<sys::File>,
    pub write: bool,
    pub slot: usize,
    pub buf: Buf,
    pub len: usize, // bytes to write
    pub off: u64,
    pub reply: Sender<Done>,
}

pub struct Disk {
    queue: Sender<Option<Job>>,
    thread: Option<sys::Joiner>,
}

impl Disk {
    pub fn start() -> Disk {
        // New 3DS: on the extra core; else (or if that fails) on the app core
        for core in [2, -2] {
            let (queue, jobs) = channel();
            if let Some(t) = sys::spawn(core, 0x2F, Box::new(move || run(jobs))) {
                return Disk { queue, thread: Some(t) };
            }
        }
        // no thread at all: jobs run inline, slower but correct
        Disk { queue: channel().0, thread: None }
    }

    pub fn submit(&self, job: Job) {
        if self.thread.is_some() {
            let _ = self.queue.send(Some(job));
        } else {
            work(job);
        }
    }

    pub fn stop(&mut self) {
        let _ = self.queue.send(None);
        if let Some(t) = self.thread.take() {
            t.join();
        }
    }
}

fn run(jobs: Receiver<Option<Job>>) {
    while let Ok(Some(job)) = jobs.recv() {
        work(job);
    }
}

fn work(job: Job) {
    let mut buf = job.buf;
    let (len, ok) = if job.write {
        match job.file.write_at(job.off, &buf[..job.len]) {
            Some(n) => (n, n == job.len),
            None => (0, false),
        }
    } else {
        match job.file.read_at(job.off, &mut buf[..]) {
            Some(n) => (n, true),
            None => (0, false),
        }
    };
    let _ = job.reply.send(Done { slot: job.slot, buf, len, ok });
}
