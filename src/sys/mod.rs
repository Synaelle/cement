// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// Platform layer. On the 3DS it is libctru (ctr.rs); on a PC it is a stand-in
// (host.rs) with the same behaviour as the old C test shim, so the protocol
// tests in test/ run against the very same server code.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(target_os = "horizon"), allow(dead_code))] // suspend/restore only come from the real APT
pub enum AptEvent {
    Suspend,
    Restore,
    Sleep,
    Wakeup,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Top,
    Bottom,
}

pub const SCREEN_TOP: u32 = 1;
pub const SCREEN_BOTTOM: u32 = 2;
pub const SCREEN_BOTH: u32 = 3;

#[cfg(target_os = "horizon")]
mod ctr;
#[cfg(target_os = "horizon")]
pub use ctr::*;

#[cfg(not(target_os = "horizon"))]
mod host;
#[cfg(not(target_os = "horizon"))]
pub use host::*;

#[cfg(not(windows))]
pub mod net_unix;
#[cfg(not(windows))]
pub use net_unix as net;

#[cfg(windows)]
pub mod net_win;
#[cfg(windows)]
pub use net_win as net;
