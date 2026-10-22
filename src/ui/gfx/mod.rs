// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The drawn screens ("Evil Twin" look). Each screen is drawn into a canvas in
// RAM when something changed; only the rows and columns that differ from what
// is shown are copied to the framebuffer.

mod bottom;
mod canvas;
mod font;

use canvas::{Canvas, Color};
use font::*;
use super::palette::Palette;
use crate::settings::DEFAULT_ACCENT;

use super::{Bottom, Top};
use crate::install::{CiaInfo, ICON, cia_info, version_text};
use crate::server::{Mode, progress_of};
use crate::sys;
use crate::util::{base_name, format_size, ip_text};
use crate::{PROTOCOL, Server, VERSION, battery_text};

// fixed whatever the accent: the switches' knob, install success and error
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
    hits: Vec<bottom::Hit>, // where the bottom screen's tabs, rows and swatches are
    pal: Palette,         // the colours for the accent below
    pal_for: u32,
    info: Option<(String, u64, Option<CiaInfo>)>, // the selected CIA's path, size and details
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            top: Some(Screen::new(sys::Screen::Top, 400)),
            bottom: Some(Screen::new(sys::Screen::Bottom, 320)),
            bottom_at: 0,
            sd_free: None,
            hits: Vec::new(),
            pal: Palette::for_accent(crate::settings::DEFAULT_ACCENT),
            pal_for: crate::settings::DEFAULT_ACCENT,
            info: None,
        }
    }

    /// works the palette out again when the accent changed (not per pixel)
    fn follow(&mut self, accent: u32) {
        if accent != self.pal_for {
            self.pal = Palette::for_accent(accent);
            self.pal_for = accent;
        }
    }

    /// what a touch at (x, y) on the bottom screen lands on
    pub fn hit(&self, x: u16, y: u16) -> Option<crate::menu::Tap> {
        let (x, y) = (x as f32, y as f32);
        self.hits.iter().find(|h| x >= h.x0 && x < h.x1 && y >= h.y0 && y < h.y1).map(|h| h.tap)
    }

    /// whether the CIA details drawn are for another file than `sel` (or none)
    pub fn info_stale(&self, sel: &Option<(String, u64)>) -> bool {
        match (&self.info, sel) {
            (None, None) => false,
            (Some((p, s, _)), Some((q, t))) => p != q || s != t,
            _ => true,
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
fn bar(c: &mut Canvas, x: f32, y: f32, w: f32, frac: Option<f32>, pal: &Palette) {
    c.pill(x, y, w, 8.0, pal.track);
    if let Some(f) = frac {
        let fw = (w * f.clamp(0.0, 1.0)).round();
        if fw > 0.0 {
            c.round(x, y, fw, 8.0, 4.0, pal.accent);
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

fn draw_card(c: &mut Canvas, x: f32, y: f32, w: f32, t: &CardText, pal: &Palette) {
    c.card(x, y, w, card_height(), 11.0, pal.card, pal.border);
    let (l, r) = (x + 1.0 + CARD_PAD_X, x + w - 1.0 - CARD_PAD_X);
    let f = &NUNITO_XBOLD_13;
    let mut ty = y + 1.0 + CARD_PAD_Y;
    let vw = f.width(&t.value, 0.0);
    let title = f.fit(&t.title, r - l - vw - 10.0);
    c.text(f, l, ty + f.ascent, &title, pal.text, 0.0);
    c.text_right(f, r, ty + f.ascent, &t.value, pal.highlight);
    ty += f.height + 6.0;
    bar(c, l, ty, r - l, t.frac, pal);
    ty += 8.0 + 6.0;
    let f = &NUNITO_BOLD_11;
    let rw = f.width(&t.right, 0.0);
    let detail = f.fit(&t.detail, r - l - rw - 10.0);
    c.text(f, l, ty + f.ascent, &detail, pal.dim, 0.0);
    c.text_right(f, r, ty + f.ascent, &t.right, pal.dim);
}

/// the section marker: a dot with a bite taken out of its top right
fn bite_dot(c: &mut Canvas, x: f32, y: f32, pal: &Palette) {
    c.circle(x + 6.0, y + 6.0, 5.5, pal.accent);
    c.circle(x + 11.0, y + 2.0, 2.6, pal.bg);
}

/// the button a hint is about
#[derive(Clone, Copy)]
enum Button {
    A,
    B,
    X,
    Dpad,
    Start,
}

/// height of the drawn keycaps
const GLYPH: f32 = 15.0;

/// width and height of a button glyph
fn glyph_size(b: Button) -> (f32, f32) {
    let f = &NUNITO_XBOLD_11;
    match b {
        Button::A => (BTN_A.size as f32, BTN_A.size as f32),
        Button::B => (BTN_B.size as f32, BTN_B.size as f32),
        Button::X => (BTN_X.size as f32, BTN_X.size as f32),
        Button::Dpad => (BTN_DPAD.size as f32, BTN_DPAD.size as f32),
        Button::Start => ((f.width("START", 0.0) + 8.0).round(), GLYPH),
    }
}

/// START has no glyph in Kenney's set: a small keycap like the
/// real buttons, with the name in it
fn keycap(c: &mut Canvas, x: f32, y: f32, name: &str, fill: Color, ink: Color) -> f32 {
    let f = &NUNITO_XBOLD_11;
    let w = (f.width(name, 0.0) + 8.0).round();
    c.pill(x, y, w, GLYPH, fill);
    c.text(f, x + 4.0, y + ((GLYPH - f.height) / 2.0).round() + f.ascent, name, ink, 0.0);
    w
}

/// a button glyph; `fg` is its light colour, `on` what is under it. Returns its width.
fn glyph(c: &mut Canvas, x: f32, y: f32, b: Button, fg: Color, on: Color, pal: &Palette) -> f32 {
    // Kenney draws in white with red marks (the pressed D-pad arms)
    let tint = |color: u32| if color == 0xffffff { fg } else { pal.accent };
    match b {
        Button::A => {
            c.icon(&BTN_A, x, y, tint);
            BTN_A.size as f32
        }
        Button::B => {
            c.icon(&BTN_B, x, y, tint);
            BTN_B.size as f32
        }
        Button::X => {
            c.icon(&BTN_X, x, y, tint);
            BTN_X.size as f32
        }
        Button::Dpad => {
            // red arms on the accent hint would vanish: they take the background there
            c.icon(&BTN_DPAD, x, y, |color| if color == 0xffffff { fg } else if on == pal.accent { on } else { pal.accent });
            BTN_DPAD.size as f32
        }
        Button::Start => keycap(c, x, y, "START", fg, on),
    }
}

/// a hint pill: a button glyph and what it does; returns its right edge
fn hint(c: &mut Canvas, x: f32, y: f32, b: Button, label: &str, bg: Color, fg: Color, pal: &Palette) -> f32 {
    let f = &NUNITO_XBOLD_11;
    let h = (f.height + 6.0).round();
    let (gw, gh) = glyph_size(b);
    let w = if label.is_empty() { 3.0 + gw + 3.0 } else { 3.0 + gw + 5.0 + f.width(label, 0.0) + 8.0 };
    c.pill(x, y, w, h, bg);
    glyph(c, x + 3.0, y + ((h - gh) / 2.0).round(), b, fg, bg, pal);
    c.text(f, x + 3.0 + gw + 5.0, y + 3.0 + f.ascent, label, fg, 0.0);
    x + w.round()
}

/// what installing the CIA would do to the installed copy
fn install_kind(info: &CiaInfo) -> &'static str {
    match (info.installed, info.version) {
        (None, _) => "new",
        (Some(_), None) => "reinstall",
        (Some(old), Some(new)) if new > old => "update",
        (Some(old), Some(new)) if new == old => "reinstall",
        _ => "downgrade",
    }
}

const TICKET_H: f32 = 42.0;

/// the selected CIA under the address: its icon, title and publisher, then
/// version, size, where it installs and its title ID, and what installing it does
fn draw_ticket(c: &mut Canvas, x: f32, y: f32, w: f32, path: &str, size: u64, info: Option<&CiaInfo>, pal: &Palette) {
    c.card(x, y, w, TICKET_H, 9.0, pal.card, pal.border);
    let ix = x + 5.0;
    let iy = (y + (TICKET_H - ICON as f32) / 2.0).round();
    match info.and_then(|i| i.icon.as_ref()) {
        Some(px) => c.image(ix, iy, ICON, px, 6.0),
        None => c.round(ix, iy, ICON as f32, ICON as f32, 6.0, pal.track),
    }
    let (l, mut r) = (ix + ICON as f32 + 8.0, x + w - 10.0);
    if let Some(info) = info {
        let kind = install_kind(info);
        let p = &NUNITO_XBOLD_11;
        let (kw, kh) = (p.width(kind, 0.0) + 14.0, p.height + 4.0);
        let accent = kind == "new" || kind == "update";
        let ky = (y + (TICKET_H - kh) / 2.0).round();
        c.pill(r - kw, ky, kw, kh, if accent { pal.accent } else { pal.track });
        c.text(p, r - kw + 7.0, ky + 2.0 + p.ascent, kind, if accent { pal.on_accent } else { pal.soft }, 0.0);
        r -= kw + 8.0;
    }
    let (f, d) = (&NUNITO_XBOLD_13, &NUNITO_BOLD_11);
    let top = (y + (TICKET_H - f.height - d.height) / 2.0).round();
    let title = info.map_or(base_name(path), |i| i.title.as_str());
    let title = f.fit(title, r - l);
    let end = c.text(f, l, top + f.ascent, &title, pal.text, 0.0);
    if let Some(i) = info.filter(|i| !i.publisher.is_empty() && end + 6.0 < r - 20.0) {
        let by = d.fit(&format!("by {}", i.publisher), r - end - 6.0);
        c.text(d, end + 6.0, top + f.ascent, &by, pal.dim, 0.0);
    }
    let detail = match info {
        Some(i) => {
            let mut parts = Vec::new();
            if let Some(v) = i.version {
                parts.push(version_text(v));
            }
            parts.push(format_size(size));
            parts.push(if i.nand { "NAND" } else { "SD" }.to_string());
            parts.push(format!("{:016X}", i.id));
            parts.join(" · ")
        }
        None => format!("{} · not a CIA Cement can read", format_size(size)),
    };
    c.text(d, l, top + f.height + d.ascent, &d.fit(&detail, r - l), pal.dim, 0.0);
}

impl Server {
    pub(super) fn draw_top(&mut self, t: &Top) {
        self.ui.follow(DEFAULT_ACCENT); // the colour from SITE COLOR is kept, not used for now
        let sel = if self.prefs.cia_info { self.selected_cia() } else { None };
        if self.ui.info_stale(&sel) {
            // read once per selection: a few KB from the SD card
            self.ui.info = sel.map(|(path, size)| {
                let info = cia_info(&path);
                (path, size, info)
            });
        }
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
        self.ui.follow(DEFAULT_ACCENT); // the colour from SITE COLOR is kept, not used for now
        if b.dirty || self.ui.sd_free.is_none() {
            self.ui.sd_free = sys::sd_space().map(|(free, _)| free);
        }
        let Some(mut scr) = self.ui.bottom.take() else { return };
        self.ui.hits = self.paint_bottom(&mut scr.c, b);
        scr.present();
        self.ui.bottom = Some(scr);
    }

    /// 400 x 240: title and battery, the address, the transfer card
    fn paint_top(&self, c: &mut Canvas, t: &Top) {
        let now = sys::now_ms();
        let pal = &self.ui.pal;
        c.fill(pal.bg);
        c.rrect(245.0, 145.0, 210.0, 170.0, [(121.8, 78.2), (88.2, 93.5), (132.3, 76.5), (77.7, 91.8)], pal.blob);

        // title row
        let (left, right) = (16.0, 384.0);
        let mut y = 14.0;
        let f = &FREDOKA_BOLD_22;
        let end = c.text(f, left, y + f.ascent, "cement", pal.text, 0.0);
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
        c.pill(right - pw, py, pw, ph, if t.low { pal.accent } else { pal.track });
        c.text(p, right - pw + 8.0, py + 3.0 + p.ascent, &label, if t.low { pal.on_accent } else { pal.highlight }, 0.0);
        // with the CIA details on, START goes up here to leave them the room
        let mut room = right - pw - 8.0;
        if self.prefs.cia_info {
            let h = &NUNITO_BOLD_11;
            let w = glyph_size(Button::Start).0 + 4.0 + h.width("exit", 0.0);
            let tx = right - pw - 10.0 - w;
            let ty = (py + (ph - GLYPH) / 2.0).round();
            let x = tx + glyph(c, tx, ty, Button::Start, pal.dim, pal.bg, pal) + 4.0;
            c.text(h, x, ty + ((GLYPH - h.height) / 2.0).round() + h.ascent, "exit", pal.dim, 0.0);
            room = tx - 8.0;
        }
        // the protocol level says which Baguette goes with this Cement (they version apart);
        // a long -dev version is cut short before the hint
        let v = &NUNITO_BOLD_11;
        let version = v.fit(&format!("v{VERSION} · protocol {PROTOCOL}"), room - end - 6.0);
        c.text(v, end + 6.0, y + f.ascent, &version, pal.dim, 0.0);
        y += f.height + 9.0;

        // address
        let f = &NUNITO_BOLD_12;
        c.text(f, left, y + f.ascent, "Connect to", pal.dim, 0.0);
        let who = match &self.login {
            Some((user, _)) => format!("login: {user}"),
            None => "no login".to_string(),
        };
        c.text_right(f, right, y + f.ascent, &f.fit(&who, 170.0), pal.dim);
        y += f.height + 2.0;
        let f = &FREDOKA_SEMI_26;
        let ip = if self.net.ip != 0 { ip_text(self.net.ip) } else { "-".into() };
        let mut x = c.text(f, left, y + f.ascent, &ip, pal.text, 1.0);
        x = c.text(f, x, y + f.ascent, ":", pal.accent, 1.0);
        c.text(f, x, y + f.ascent, &sys::ports().0.to_string(), pal.text, 1.0);
        y += f.height + 2.0;
        // the buttons as small keycaps, then what they do, one per line; a low
        // battery warning on the right of them, one line each too (START is in
        // the title row with the CIA details on)
        let warning = ["Battery low: no new transfers", "or installs until you charge"];
        let f = &NUNITO_BOLD_11;
        let ticket = self.prefs.cia_info.then_some(self.ui.info.as_ref()).flatten();
        let start = if self.prefs.cia_info { None } else { Some((Button::Start, "exit")) };
        if let (Some((path, size, info)), false) = (ticket, t.low) {
            draw_ticket(c, left, y + 4.0, right - left, path, *size, info.as_ref(), pal);
        } else if start.is_some() || t.low {
            for (i, hint) in [start, None].into_iter().enumerate() {
                if let Some((button, what)) = hint {
                    let gy = (y + (f.height - GLYPH) / 2.0).round();
                    let x = left + glyph(c, left, gy, button, pal.dim, pal.bg, pal) + 4.0;
                    c.text(f, x, y + f.ascent, what, pal.dim, 0.0);
                }
                if t.low {
                    let w = &NUNITO_XBOLD_12;
                    c.text_right(w, right, y + (f.height - w.height) / 2.0 + w.ascent, warning[i], pal.highlight);
                }
                y += f.height.max(GLYPH) + 3.0;
            }
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
            card.detail = if self.prefs.ftp { "No transfer right now" } else { "Turn it back on in Settings, on the bottom screen" }.into();
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
        draw_card(c, left, 226.0 - card_height(), right - left, &card, pal);
    }
}
