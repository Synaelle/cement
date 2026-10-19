// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The drawn screens ("Evil Twin" look). Each screen is drawn into a canvas in
// RAM when something changed; only the rows and columns that differ from what
// is shown are copied to the framebuffer.

mod canvas;
mod font;

use canvas::{Canvas, Color};
use font::*;

use super::{Bottom, Top};
use crate::server::{CTRL_PORT, Mode, progress_of};
use crate::sys;
use crate::util::{base_name, format_size, ip_text};
use crate::{Server, VERSION, battery_text};

const BG: Color = 0x0e0a0a;
const BLOB: Color = 0x1a1112;
const CARD: Color = 0x1d1414;
const BORDER: Color = 0x4d1d21;
const TRACK: Color = 0x2a1516;
const SELECTED: Color = 0x3d1418;
const TEXT: Color = 0xf2e7e4;
const DIM: Color = 0xa8928e;
const HIGHLIGHT: Color = 0xd8676a;
const SOFT: Color = 0xe9a9a3;
const WHITE: Color = 0xffffff;
const GOOD: Color = 0x8ccf9a;
const BAD: Color = 0xe5484d;

/// the bottom screen stays lit (until the idle timer), with an empty state
pub const BOTTOM_ALWAYS_ON: bool = true;

/// a pixel value never drawn: everything differs from it
const UNKNOWN: u32 = 0xFF00_0000;

/// bottom screen redraws during an install
const INSTALL_FRAME_MS: u64 = 100;

struct Screen {
    which: sys::Screen,
    c: Canvas,
    shown: Vec<u32>,                         // what the displayed framebuffer holds
    last: Option<(usize, usize, usize, usize)>, // changed at the last present: the other buffer lacks it
}

impl Screen {
    fn new(which: sys::Screen, w: usize) -> Screen {
        Screen { which, c: Canvas::new(w, 240), shown: vec![UNKNOWN; w * 240], last: Some((0, 0, w, 240)) }
    }

    fn invalidate(&mut self) {
        self.shown.fill(UNKNOWN);
        self.last = Some((0, 0, self.c.w, self.c.h));
    }

    /// copies what changed since the last present (and what the back buffer
    /// missed from it: the screens are double buffered) and swaps
    fn present(&mut self) {
        let (w, h) = (self.c.w, self.c.h);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            let (a, b) = (&self.c.px[y * w..][..w], &self.shown[y * w..][..w]);
            if a == b {
                continue;
            }
            let first = (0..w).find(|&x| a[x] != b[x]).unwrap();
            let last = (0..w).rfind(|&x| a[x] != b[x]).unwrap();
            (x0, x1) = (x0.min(first), x1.max(last + 1));
            (y0, y1) = (y0.min(y), y + 1);
        }
        if x1 <= x0 {
            return;
        }
        let (mut u0, mut v0, mut u1, mut v1) = (x0, y0, x1, y1);
        if let Some((a, b, c, d)) = self.last {
            (u0, v0, u1, v1) = (u0.min(a), v0.min(b), u1.max(c), v1.max(d));
        }
        sys::present(self.which, &self.c.px, u0, v0, u1, v1);
        for y in y0..y1 {
            self.shown[y * w + x0..y * w + x1].copy_from_slice(&self.c.px[y * w + x0..y * w + x1]);
        }
        self.last = Some((x0, y0, x1, y1));
    }
}

pub struct Ui {
    top: Option<Screen>, // taken out while drawing
    bottom: Option<Screen>,
    bottom_at: u64,       // last bottom screen drawing
    sd_free: Option<u64>, // read again when the CIA list changes
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            top: Some(Screen::new(sys::Screen::Top, 400)),
            bottom: Some(Screen::new(sys::Screen::Bottom, 320)),
            bottom_at: 0,
            sd_free: None,
        }
    }

    /// after the HOME menu or sleep: the framebuffers hold something else
    pub fn invalidate(&mut self) {
        for s in [&mut self.top, &mut self.bottom].into_iter().flatten() {
            s.invalidate();
        }
    }
}

/// "12.3 GB" for the free space
fn format_big(v: u64) -> String {
    if v >= 1 << 30 { format!("{:.1} GB", v as f64 / (1u64 << 30) as f64) } else { format_size(v) }
}

fn pcs(n: usize) -> String {
    format!("{n} PC{} connected", if n == 1 { "" } else { "s" })
}

fn time_left(secs: i64) -> String {
    if secs >= 0 { format!("{}:{:02} left", secs / 60, secs % 60) } else { String::new() }
}

/// a progress bar, `frac` of it filled
fn bar(c: &mut Canvas, x: f32, y: f32, w: f32, frac: Option<f32>, accent: Color) {
    c.pill(x, y, w, 8.0, TRACK);
    if let Some(f) = frac {
        let fw = (w * f.clamp(0.0, 1.0)).round();
        if fw > 0.0 {
            c.round(x, y, fw, 8.0, 4.0, accent);
        }
    }
}

/// the card shared by the transfer (top) and the install (bottom): a title
/// with a value on its right, a bar, a detail line and a time on its right
struct CardText {
    title: String,
    value: String,
    frac: Option<f32>,
    detail: String,
    right: String,
}

const CARD_PAD_Y: f32 = 9.0;
const CARD_PAD_X: f32 = 11.0;

fn card_height() -> f32 {
    2.0 + 2.0 * CARD_PAD_Y + NUNITO_XBOLD_13.height + 6.0 + 8.0 + 6.0 + NUNITO_BOLD_11.height
}

fn draw_card(c: &mut Canvas, x: f32, y: f32, w: f32, t: &CardText, accent: Color) {
    c.card(x, y, w, card_height(), 11.0, CARD, BORDER);
    let (l, r) = (x + 1.0 + CARD_PAD_X, x + w - 1.0 - CARD_PAD_X);
    let f = &NUNITO_XBOLD_13;
    let mut ty = y + 1.0 + CARD_PAD_Y;
    let vw = f.width(&t.value, 0.0);
    let title = f.fit(&t.title, r - l - vw - 10.0);
    c.text(f, l, ty + f.ascent, &title, TEXT, 0.0);
    c.text_right(f, r, ty + f.ascent, &t.value, HIGHLIGHT);
    ty += f.height + 6.0;
    bar(c, l, ty, r - l, t.frac, accent);
    ty += 8.0 + 6.0;
    let f = &NUNITO_BOLD_11;
    let rw = f.width(&t.right, 0.0);
    let detail = f.fit(&t.detail, r - l - rw - 10.0);
    c.text(f, l, ty + f.ascent, &detail, DIM, 0.0);
    c.text_right(f, r, ty + f.ascent, &t.right, DIM);
}

/// the section marker: a dot with a bite taken out of its top right
fn bite_dot(c: &mut Canvas, x: f32, y: f32, accent: Color) {
    c.circle(x + 6.0, y + 6.0, 5.5, accent);
    c.circle(x + 11.0, y + 2.0, 2.6, BG);
}

/// the button a hint is about
#[derive(Clone, Copy)]
enum Button {
    A,
    Dpad,
    Select,
    Start,
}

/// height of the drawn keycaps
const GLYPH: f32 = 15.0;

/// width and height of a button glyph
fn glyph_size(b: Button) -> (f32, f32) {
    let f = &NUNITO_XBOLD_11;
    match b {
        Button::A => (BTN_A.size as f32, BTN_A.size as f32),
        Button::Dpad => (BTN_DPAD.size as f32, BTN_DPAD.size as f32),
        Button::Select => ((f.width("SELECT", 0.0) + 8.0).round(), GLYPH),
        Button::Start => ((f.width("START", 0.0) + 8.0).round(), GLYPH),
    }
}

/// SELECT and START have no glyph in Kenney's set: a small keycap like the
/// real buttons, with the name in it
fn keycap(c: &mut Canvas, x: f32, y: f32, name: &str, fill: Color, ink: Color) -> f32 {
    let f = &NUNITO_XBOLD_11;
    let w = (f.width(name, 0.0) + 8.0).round();
    c.pill(x, y, w, GLYPH, fill);
    c.text(f, x + 4.0, y + ((GLYPH - f.height) / 2.0).round() + f.ascent, name, ink, 0.0);
    w
}

/// a button glyph; `fg` is its light colour, `on` what is under it. Returns its width.
fn glyph(c: &mut Canvas, x: f32, y: f32, b: Button, fg: Color, on: Color, accent: Color) -> f32 {
    // Kenney draws in white with red marks (the pressed D-pad arms)
    let tint = |color: u32| if color == 0xffffff { fg } else { accent };
    match b {
        Button::A => {
            c.icon(&BTN_A, x, y, tint);
            BTN_A.size as f32
        }
        Button::Dpad => {
            // red arms on the accent hint would vanish: they take the background there
            c.icon(&BTN_DPAD, x, y, |color| if color == 0xffffff { fg } else if on == accent { on } else { accent });
            BTN_DPAD.size as f32
        }
        Button::Select => keycap(c, x, y, "SELECT", fg, on),
        Button::Start => keycap(c, x, y, "START", fg, on),
    }
}

/// a hint pill: a button glyph and what it does; returns its right edge
fn hint(c: &mut Canvas, x: f32, y: f32, b: Button, label: &str, bg: Color, fg: Color, accent: Color) -> f32 {
    let f = &NUNITO_XBOLD_11;
    let h = (f.height + 6.0).round();
    let (gw, gh) = glyph_size(b);
    let w = 3.0 + gw + 5.0 + f.width(label, 0.0) + 8.0;
    c.pill(x, y, w, h, bg);
    glyph(c, x + 3.0, y + ((h - gh) / 2.0).round(), b, fg, bg, accent);
    c.text(f, x + 3.0 + gw + 5.0, y + 3.0 + f.ascent, label, fg, 0.0);
    x + w.round()
}

impl Server {
    pub(super) fn draw_top(&mut self, t: &Top) {
        let Some(mut scr) = self.ui.top.take() else { return };
        self.paint_top(&mut scr.c, t);
        scr.present();
        self.ui.top = Some(scr);
    }

    pub(super) fn draw_bottom(&mut self, b: &Bottom) {
        let now = sys::now_ms();
        if !b.dirty && now.saturating_sub(self.ui.bottom_at) < INSTALL_FRAME_MS {
            return;
        }
        self.ui.bottom_at = now;
        if b.dirty || self.ui.sd_free.is_none() {
            self.ui.sd_free = sys::sd_space().map(|(free, _)| free);
        }
        let Some(mut scr) = self.ui.bottom.take() else { return };
        self.paint_bottom(&mut scr.c, b);
        scr.present();
        self.ui.bottom = Some(scr);
    }

    /// 400 x 240: title and battery, the address, the transfer card
    fn paint_top(&self, c: &mut Canvas, t: &Top) {
        let now = sys::now_ms();
        c.fill(BG);
        c.rrect(245.0, 145.0, 210.0, 170.0, [(121.8, 78.2), (88.2, 93.5), (132.3, 76.5), (77.7, 91.8)], BLOB);

        // title row
        let (left, right) = (16.0, 384.0);
        let mut y = 14.0;
        let f = &FREDOKA_BOLD_22;
        let end = c.text(f, left, y + f.ascent, "cement", TEXT, 0.0);
        c.text(&NUNITO_BOLD_11, end + 6.0, y + f.ascent, &format!("v{VERSION}"), DIM, 0.0);
        let bat = t.bat;
        let label = format!(
            "{}{}",
            battery_text(bat),
            if bat.charging {
                " charging"
            } else if bat.plugged {
                " plugged in"
            } else {
                ""
            }
        );
        let p = &NUNITO_XBOLD_12;
        let (pw, ph) = (p.width(&label, 0.0) + 16.0, p.height + 6.0);
        let py = y + (f.height - ph) / 2.0;
        c.pill(right - pw, py, pw, ph, if t.low { self.accent } else { TRACK });
        c.text(p, right - pw + 8.0, py + 3.0 + p.ascent, &label, if t.low { WHITE } else { HIGHLIGHT }, 0.0);
        y += f.height + 9.0;

        // address
        let f = &NUNITO_BOLD_12;
        c.text(f, left, y + f.ascent, "Connect Chomp to", DIM, 0.0);
        y += f.height + 2.0;
        let f = &FREDOKA_SEMI_26;
        let ip = if self.net.ip != 0 { ip_text(self.net.ip) } else { "-".into() };
        let mut x = c.text(f, left, y + f.ascent, &ip, TEXT, 1.0);
        x = c.text(f, x, y + f.ascent, " : ", self.accent, 1.0);
        c.text(f, x, y + f.ascent, &CTRL_PORT.to_string(), TEXT, 1.0);
        y += f.height + 2.0;
        let f = &NUNITO_BOLD_11;
        // the buttons as small keycaps, then what they do
        let gy = (y + (f.height - GLYPH) / 2.0).round();
        let mut x = left;
        x += glyph(c, x, gy, Button::Start, DIM, BG, self.accent) + 4.0;
        x = c.text(f, x, y + f.ascent, "exit", DIM, 0.0) + 10.0;
        x += glyph(c, x, gy, Button::Select, DIM, BG, self.accent) + 4.0;
        x = c.text(f, x, y + f.ascent, "top screen on/off", DIM, 0.0) + 10.0;
        c.text(f, x, y + f.ascent, "close the lid to sleep", DIM, 0.0);
        y += f.height;

        if t.low {
            let f = &NUNITO_XBOLD_12;
            y += 6.0;
            c.text(f, left, y + f.ascent, "Battery low: no new transfers or installs until you charge", HIGHLIGHT, 0.0);
        }

        // transfer card
        let batch = self.sess.iter().find(|s| s.ctrl.is_some() && s.batch_files != 0);
        let bp = batch.map(|s| progress_of(s, now));
        let mut card = CardText { title: String::new(), value: String::new(), frac: None, detail: String::new(), right: String::new() };
        let mut parts = Vec::new();
        if let Some(s) = batch {
            let (done, total) = (s.batch_files_done, s.batch_files);
            parts.push(if s.batch_kind == "delete" {
                format!("{done} of {total} files")
            } else {
                format!("file {} of {total}", (done + 1).min(total))
            });
        }
        if let Some(i) = t.active {
            let s = &self.sess[i];
            let p = progress_of(s, now);
            let verb = if s.mode == Mode::Stor { "Receiving" } else { "Sending" };
            card.title = format!("{verb} {}", s.name);
            card.value = format!("{}/s", format_size(self.rate));
            if p.size != 0 {
                card.frac = Some(p.done as f32 / p.size as f32);
            }
            if batch.is_none() {
                parts.push(if p.size != 0 {
                    format!("{} of {}", format_size(p.done), format_size(p.size))
                } else {
                    format_size(p.done)
                });
                card.right = time_left(p.eta);
            }
        } else if let Some(s) = batch {
            card.title = match s.batch_kind.as_str() {
                "delete" => "Deleting files",
                "down" => "Sending files",
                _ => "Receiving files",
            }
            .into();
            if s.batch_kind == "delete" {
                card.frac = Some(s.batch_files_done as f32 / s.batch_files.max(1) as f32);
            }
        } else {
            let up = self.net.up && self.net.listen.is_some();
            card.title = if up && t.clients != 0 { pcs(t.clients) } else { self.status.clone() };
            card.detail = "No transfer right now".into();
        }
        if let (Some(s), Some(bp)) = (batch, &bp) {
            if s.batch_kind != "delete" && bp.batch_bytes != 0 && t.active.is_none() {
                card.frac = Some(bp.batch_done as f32 / bp.batch_bytes as f32);
            }
            card.right = time_left(bp.batch_eta);
        }
        if t.active.is_some() || batch.is_some() {
            if t.clients != 0 {
                parts.push(pcs(t.clients));
            }
            card.detail = parts.join(" · ");
        }
        draw_card(c, left, 226.0 - card_height(), right - left, &card, self.accent);
    }

    /// 320 x 240: the CIA list, the install card, the last message, the buttons
    fn paint_bottom(&self, c: &mut Canvas, b: &Bottom) {
        c.fill(BG);
        let (left, right) = (13.0, 307.0);

        // header
        let top = 12.0;
        let f = &FREDOKA_BOLD_16;
        let base = top + f.ascent;
        bite_dot(c, left, base + 0.5 - 12.0, self.accent);
        c.text(f, left + 15.0, base, "CIAs", TEXT, 0.0);
        if let Some(free) = self.ui.sd_free {
            let s = &NUNITO_BOLD_11;
            c.text_right(s, right, top + (f.height - s.height) / 2.0 + s.ascent, &format!("SD {} free", format_big(free)), DIM);
        }
        let list_top = top + f.height + 6.0;

        // from the bottom up: buttons, message, install card
        let f = &NUNITO_XBOLD_11;
        let hy = 228.0 - (f.height + 6.0);
        let first = if b.busy && !self.installing() { "waits for transfers" } else { "install" };
        let a = self.accent;
        let mut x = hint(c, left, hy, Button::A, first, a, WHITE, a) + 5.0;
        x = hint(c, x, hy, Button::Dpad, "pick", TRACK, SOFT, a) + 5.0;
        hint(c, x, hy, Button::Select, "screen off", TRACK, SOFT, a);
        let mut bottom = hy - 6.0;

        if b.show_msg {
            let f = &NUNITO_XBOLD_12;
            bottom -= f.height;
            let color = if self.cia_msg.starts_with("Installed") { GOOD } else { BAD };
            c.text(f, left, bottom + f.ascent, &f.fit(&self.cia_msg, right - left), color, 0.0);
            bottom -= 6.0;
        }

        if let Some(inst) = &self.inst {
            let done = *inst.shared.done.lock().unwrap();
            let pct = if inst.total != 0 { done * 100 / inst.total } else { 0 };
            let card = CardText {
                title: format!("Installing {}", base_name(&inst.path)),
                value: format!("{pct}%"),
                frac: (inst.total != 0).then(|| done as f32 / inst.total as f32),
                detail: format!("{} of {}", format_size(done), format_size(inst.total)),
                right: String::new(),
            };
            bottom -= card_height();
            draw_card(c, left, bottom, right - left, &card, self.accent);
            bottom -= 6.0;
        }

        if self.cias.is_empty() && self.inst.is_none() {
            // empty state
            let h = (bottom - list_top).min(64.0);
            c.round(left, list_top, right - left, h, 9.0, CARD);
            let (a, b) = (&NUNITO_XBOLD_13, &NUNITO_BOLD_12);
            let y = list_top + (h - a.height - 2.0 - b.height) / 2.0;
            let mid = (left + right) / 2.0;
            let line1 = "No CIAs yet";
            let line2 = "Send one from Chomp to install it here";
            c.text(a, mid - a.width(line1, 0.0) / 2.0, y + a.ascent, line1, TEXT, 0.0);
            c.text(b, mid - b.width(line2, 0.0) / 2.0, y + a.height + 2.0 + b.ascent, line2, DIM, 0.0);
            return;
        }

        // the list: as many rows as fit, scrolled to keep the selection in view
        let row_h = NUNITO_XBOLD_13.height + 14.0;
        let room = bottom - list_top + 6.0 - 2.0; // the selected row has a border
        let rows = ((room / (row_h + 6.0)).floor() as usize).max(1);
        let first = if self.cia_sel >= rows { self.cia_sel - rows + 1 } else { 0 };
        let mut y = list_top;
        for (i, cia) in self.cias.iter().enumerate().skip(first).take(rows) {
            let sel = i == self.cia_sel;
            let (h, inset) = if sel { (row_h + 2.0, 1.0) } else { (row_h, 0.0) };
            if sel {
                c.card(left, y, right - left, h, 9.0, SELECTED, self.accent);
            } else {
                c.round(left, y, right - left, h, 9.0, CARD);
            }
            let f = if sel { &NUNITO_XBOLD_13 } else { &NUNITO_BOLD_13 };
            let base = y + inset + 7.0 + f.ascent;
            let (l, r) = (left + inset + 9.0, right - inset - 9.0);
            let size = format_size(cia.size);
            let name = f.fit(base_name(&cia.path), r - l - f.width(&size, 0.0) - 10.0);
            c.text(f, l, base, &name, TEXT, 0.0);
            c.text_right(f, r, base, &size, if sel { SOFT } else { DIM });
            y += h + 6.0;
        }
    }
}
