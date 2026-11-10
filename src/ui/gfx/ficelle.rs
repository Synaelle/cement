// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: AGPL-3.0-or-later
// The Ficelle look: the red look's colours (so the synced accent too), Hanken
// Grotesk at 400 and 600, sharp 2 px corners, 1 px borders and thin lines. Its
// own layout of both screens, showing the same things as the round looks.

use super::bottom::{Hit, RowValue, UpdateNote, row_label, window};
use super::canvas::{Canvas, Color};
use super::font::*;
use super::{BAD, Button, CardText, GOOD, help, install_kind, ticket_detail, wrap};
use crate::install::{CiaInfo, ICON};
use crate::menu::{ROWS, Row, TABS, Tab, Tap};
use crate::picture::is_image;
use crate::ui::palette::Palette;
use crate::ui::{Bottom, Top};
use crate::util::{base_name, format_size, ip_text, is_cia};
use crate::{PROTOCOL, Server, VERSION, battery_text, sys};

const TAB_H: f32 = 28.0; // the tab bar, its line under it
const HINT_TOP: f32 = 209.0; // the line over the hints
const L: f32 = 6.0; // list rows, edge to edge
const R: f32 = 314.0;

/// `a` moved towards `b` by `t`
fn mix(a: Color, b: Color, t: f32) -> Color {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 255) as f32, ((b >> s) & 255) as f32);
        ((x + (y - x) * t).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// a border that must show (border-strong)
fn strong(pal: &Palette) -> Color {
    mix(pal.border, pal.text, 0.1)
}

/// a 1 px line
fn hline(c: &mut Canvas, x: f32, y: f32, w: f32, color: Color) {
    c.round(x, y, w, 1.0, 0.0, color);
}

fn vline(c: &mut Canvas, x: f32, y: f32, h: f32, color: Color) {
    c.round(x, y, 1.0, h, 0.0, color);
}

/// the baseline that centres a capital's ink on `cy`
fn cap_baseline(f: &Font, ch: char, cy: f32) -> f32 {
    let g = f.glyph(ch);
    (cy - g.top as f32 - g.h as f32 / 2.0).round()
}

/// the baseline that centres a line of `f` on `cy`
fn mid(f: &Font, cy: f32) -> f32 {
    (cy - f.height / 2.0).round() + f.ascent
}

/// a CIA, 13 x 14 pixels
const PACKAGE: [&str; 14] = [
    "......#......",
    "....##.##....",
    "..##.....##..",
    "##.........##",
    "#.##.....##.#",
    "#...##.##...#",
    "#.....#.....#",
    "#.....#.....#",
    "#.....#.....#",
    "#.....#.....#",
    "##....#....##",
    "..##..#..##..",
    "....##.##....",
    "......#......",
];

/// the icons of the lists, 14 px, 1 px strokes on `bg`
#[derive(Clone, Copy)]
enum Glyph {
    Package,
    Folder,
    Image,
    File,
}

fn draw_glyph(c: &mut Canvas, x: f32, y: f32, g: Glyph, ink: Color, bg: Color) {
    match g {
        Glyph::Package => {
            // a box seen from a corner, like Lucide's package
            for (row, line) in PACKAGE.iter().enumerate() {
                for (col, px) in line.bytes().enumerate() {
                    if px == b'#' {
                        c.round(x + col as f32, y + row as f32, 1.0, 1.0, 0.0, ink);
                    }
                }
            }
        }
        Glyph::Folder => {
            c.card(x, y + 1.0, 6.0, 4.0, 1.0, bg, ink);
            c.card(x, y + 3.0, 14.0, 10.0, 2.0, bg, ink);
        }
        Glyph::Image => {
            c.card(x, y + 2.0, 14.0, 11.0, 2.0, bg, ink);
            c.circle(x + 4.5, y + 5.5, 1.3, ink);
            for (dx, dy) in [(2.0, 10.0), (3.0, 9.0), (4.0, 8.0), (5.0, 7.0), (6.0, 8.0), (7.0, 9.0), (8.0, 10.0), (9.0, 9.0), (10.0, 8.0), (11.0, 9.0)] {
                c.round(x + dx, y + dy, 1.0, 1.0, 0.0, ink);
            }
        }
        Glyph::File => {
            c.card(x + 2.0, y + 1.0, 10.0, 12.0, 2.0, bg, ink);
            hline(c, x + 4.0, y + 6.0, 6.0, ink);
            hline(c, x + 4.0, y + 9.0, 6.0, ink);
        }
    }
}

/// a sharp on/off switch whose right edge is at `right`, on `bg`
fn switch(c: &mut Canvas, right: f32, cy: f32, on: bool, bg: Color, pal: &Palette) {
    let (w, h) = (22.0, 12.0);
    let (x, y) = (right - w, (cy - h / 2.0).round());
    if on {
        c.round(x, y, w, h, 2.0, pal.accent);
        c.round(x + w - 10.0, y + 2.0, 8.0, 8.0, 1.0, pal.on_accent);
    } else {
        c.card(x, y, w, h, 2.0, bg, strong(pal));
        c.round(x + 2.0, y + 2.0, 8.0, 8.0, 1.0, pal.dim);
    }
}

/// a button of the hint bar, 15 px round: filled in the accent for the main one,
/// outlined for the others. Returns its width.
fn button(c: &mut Canvas, x: f32, cy: f32, b: Button, main: bool, pal: &Palette) -> f32 {
    let d = 15.0;
    let cx = x + d / 2.0;
    let ink = if main { pal.on_accent } else { pal.dim };
    if main {
        c.circle(cx, cy, d / 2.0, pal.accent);
    } else {
        c.circle(cx, cy, d / 2.0, strong(pal));
        c.circle(cx, cy, d / 2.0 - 1.0, pal.bg);
    }
    let name = match b {
        Button::A => 'A',
        Button::B => 'B',
        Button::X => 'X',
        Button::Start => 'S',
        Button::Dpad => {
            // the four directions: a thin cross
            hline(c, cx - 3.5, cy.round() - 0.5, 7.0, ink);
            vline(c, cx.round() - 0.5, cy - 3.5, 7.0, ink);
            return d;
        }
    };
    letter(c, cx, cy, name, ink);
    d
}

fn letter(c: &mut Canvas, cx: f32, cy: f32, ch: char, ink: Color) {
    let f = &HANKEN_SEMI_11;
    let s = ch.to_string();
    let w = f.width(&s, 0.0);
    c.text(f, (cx - w / 2.0).round(), cap_baseline(f, ch, cy), &s, ink, 0.0);
}

/// a card: 1 px border, 2 px corners, on the surface colour
fn card_height() -> f32 {
    1.0 + 9.0 + HANKEN_SEMI_13.height + 6.0 + 2.0 + 6.0 + HANKEN_REG_12.height + 9.0 + 1.0
}

fn draw_card(c: &mut Canvas, x: f32, y: f32, w: f32, t: &CardText, pal: &Palette) {
    c.card(x, y, w, card_height(), 2.0, pal.card, pal.border);
    let (l, r) = (x + 1.0 + 12.0, x + w - 1.0 - 12.0);
    let (f, s) = (&HANKEN_SEMI_13, &HANKEN_REG_12);
    let mut ty = y + 1.0 + 9.0;
    let vw = s.width(&t.value, 0.0);
    c.text(f, l, ty + f.ascent, &f.fit(&t.title, r - l - vw - 10.0), pal.text, 0.0);
    c.text_right(s, r, ty + f.ascent, &t.value, pal.dim);
    ty += f.height + 6.0;
    hline(c, l, ty, r - l, pal.border);
    hline(c, l, ty + 1.0, r - l, pal.border);
    if let Some(frac) = t.frac {
        let fw = ((r - l) * frac.clamp(0.0, 1.0)).round();
        if fw > 0.0 {
            c.round(l, ty, fw, 2.0, 0.0, pal.accent);
        }
    }
    ty += 2.0 + 6.0;
    let rw = s.width(&t.right, 0.0);
    c.text(s, l, ty + s.ascent, &s.fit(&t.detail, r - l - rw - 10.0), pal.dim, 0.0);
    c.text_right(s, r, ty + s.ascent, &t.right, pal.dim);
}

/// the CIA's icon at 32 px (averaged down from 48)
fn small_icon(px: &[u32]) -> Vec<u32> {
    let (n, s) = (32usize, ICON);
    let mut out = vec![0u32; n * n];
    for y in 0..n {
        for x in 0..n {
            // each small pixel covers 1.5 x 1.5 big ones: average the 2 x 2 around its centre
            let (fx, fy) = (x * s / n, y * s / n);
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = px[(fy + dy).min(s - 1) * s + (fx + dx).min(s - 1)];
                (r, g, b) = (r + (p >> 16 & 255), g + (p >> 8 & 255), b + (p & 255));
            }
            out[y * n + x] = (r / 4) << 16 | (g / 4) << 8 | (b / 4);
        }
    }
    out
}

/// the image viewer's message: opening, or why it can't show it
pub(super) fn paint_note(c: &mut Canvas, title: &str, detail: &str, pal: &Palette) {
    c.fill(pal.bg);
    let (f, d) = (&HANKEN_SEMI_13, &HANKEN_REG_12);
    let w = c.w as f32;
    let h = if detail.is_empty() { f.height } else { f.height + 4.0 + d.height };
    let y = ((c.h as f32 - h) / 2.0).round();
    let title = f.fit(title, w - 40.0);
    c.text(f, ((w - f.width(&title, 0.0)) / 2.0).round(), y + f.ascent, &title, pal.text, 0.0);
    if !detail.is_empty() {
        let detail = d.fit(detail, w - 40.0);
        c.text(d, ((w - d.width(&detail, 0.0)) / 2.0).round(), y + f.height + 4.0 + d.ascent, &detail, pal.dim, 0.0);
    }
}

impl Server {
    /// 400 x 240: name, version and battery; the address; what's selected; the transfer card
    pub(super) fn paint_top_ficelle(&self, c: &mut Canvas, t: &Top) {
        let now = sys::now_ms();
        let pal = &self.ui.pal;
        c.fill(pal.bg);
        let (left, right) = (16.0, 384.0);

        // name and version, battery on the right
        let mut y = 14.0;
        let (n, s) = (&HANKEN_SEMI_18, &HANKEN_REG_12);
        let base = y + n.ascent;
        let end = c.text(n, left, base, "Cement", pal.text, 0.0);
        let b = &HANKEN_REG_13;
        let pct = battery_text(t.bat);
        let mut x = right - b.width(&pct, 0.0);
        c.text(b, x, base, &pct, if t.low { pal.accent } else { pal.text }, 0.0);
        // the battery: an outline, its bars in green (red when low)
        x -= 6.0 + 16.0;
        let by = (y + (n.height - 10.0) / 2.0).round();
        c.card(x, by, 14.0, 10.0, 2.0, pal.bg, pal.dim);
        c.round(x + 14.0, by + 3.0, 2.0, 4.0, 0.0, pal.dim);
        let bars = if t.bat.percent >= 0 { (t.bat.percent + 32) / 33 } else { t.bat.bars.max(0) * 3 / 5 };
        for i in 0..bars.clamp(0, 3) {
            c.round(x + 3.0 + i as f32 * 3.0, by + 3.0, 2.0, 4.0, 0.0, if t.low { pal.accent } else { GOOD });
        }
        if t.bat.charging || t.bat.plugged {
            let what = if t.bat.charging { "charging" } else { "plugged in" };
            x -= 6.0 + s.width(what, 0.0);
            c.text(s, x, base, what, pal.dim, 0.0);
        }
        // START exits: an outlined key, quiet
        let k = &HANKEN_SEMI_11;
        let kw = (k.width("START", 0.0) + 8.0).round();
        let ew = s.width("exit", 0.0);
        x -= 12.0 + ew;
        c.text(s, x, base, "exit", pal.dim, 0.0);
        x -= 4.0 + kw;
        let ky = (y + (n.height - 14.0) / 2.0).round();
        c.card(x, ky, kw, 14.0, 2.0, pal.bg, strong(pal));
        c.text(k, x + 4.0, cap_baseline(k, 'S', ky + 7.0), "START", pal.dim, 0.0);
        // the protocol level says which Baguette goes with this Cement
        let version = s.fit(&format!("{VERSION} · protocol {PROTOCOL}"), x - 10.0 - end - 8.0);
        c.text(s, end + 8.0, base, &version, pal.dim, 0.0);
        y += n.height + 10.0;

        // the address
        c.text(s, left, y + s.ascent, "Connect to", pal.dim, 0.0);
        let who = match &self.login {
            Some((user, _)) => format!("login: {user}"),
            None => "no login".to_string(),
        };
        c.text_right(s, right, y + s.ascent, &s.fit(&who, 170.0), pal.dim);
        y += s.height + 1.0;
        let a = &HANKEN_SEMI_30;
        let ip = if self.net.ip != 0 { ip_text(self.net.ip) } else { "-".into() };
        let mut x = c.text(a, left, y + a.ascent, &ip, pal.text, 0.0);
        x = c.text(a, x, y + a.ascent, ":", pal.accent, 0.0);
        c.text(a, x, y + a.ascent, &sys::ports().0.to_string(), pal.text, 0.0);
        y += a.height + 2.0;

        // the transfer card at the bottom, what's selected between
        let card_y = 240.0 - 14.0 - card_height();
        let room = (y, card_y - 6.0);
        let ticket = self.prefs.cia_info.then_some(self.ui.info.as_ref()).flatten();
        if t.low {
            let w = &HANKEN_SEMI_12;
            let mut ly = room.0;
            for line in ["Battery low: no new transfers or installs", "until you charge"] {
                c.text(w, left, ly + w.ascent, line, pal.accent, 0.0);
                ly += w.height;
            }
        } else if self.menu.tab == Tab::Settings {
            let row = ROWS[self.menu.row];
            let text = if row == Row::Update { self.update_help() } else { help(row).to_string() };
            let f = &HANKEN_REG_12;
            let mut lines = wrap(f, &text, right - left);
            if lines.len() > 2 {
                let rest = lines[1..].join(" ");
                lines = vec![lines[0].clone(), f.fit(&rest, right - left)];
            }
            let top = (room.0 + (room.1 - room.0 - f.height * lines.len() as f32) / 2.0).round();
            for (i, line) in lines.iter().enumerate() {
                c.text(f, left, top + i as f32 * f.height + f.ascent, line, pal.dim, 0.0);
            }
        } else if let Some((path, size, info)) = ticket {
            self.ticket(c, left, right, room, path, *size, info.as_ref(), pal);
        }

        draw_card(c, left, card_y, right - left, &self.transfer_card(t, now), pal);
    }

    /// the selected CIA: its icon, title and publisher, its details, and what installing it does
    fn ticket(&self, c: &mut Canvas, left: f32, right: f32, room: (f32, f32), path: &str, size: u64, info: Option<&CiaInfo>, pal: &Palette) {
        let (f, d) = (&HANKEN_SEMI_13, &HANKEN_REG_12);
        let icon = 32.0;
        let iy = (room.0 + (room.1 - room.0 - icon) / 2.0).round();
        match info.and_then(|i| i.icon.as_ref()) {
            Some(px) => c.image(left, iy, 32, &small_icon(px), 2.0),
            None => c.card(left, iy, icon, icon, 2.0, pal.card, pal.border),
        }
        let l = left + icon + 10.0;
        let mut r = right;
        if let Some(info) = info {
            let (kind, accent) = install_kind(info);
            let k = &HANKEN_SEMI_12;
            let kw = k.width(kind, 0.0);
            c.text(k, r - kw, mid(k, iy + icon / 2.0), kind, if accent { pal.accent } else { pal.dim }, 0.0);
            r -= kw + 12.0;
        }
        let top = (iy + (icon - f.height - d.height) / 2.0).round();
        let title = info.map_or(base_name(path), |i| i.title.as_str());
        let end = c.text(f, l, top + f.ascent, &f.fit(title, r - l), pal.text, 0.0);
        if let Some(i) = info.filter(|i| !i.publisher.is_empty() && end + 6.0 < r - 20.0) {
            let by = d.fit(&format!("by {}", i.publisher), r - end - 6.0);
            c.text(d, end + 6.0, top + f.ascent, &by, pal.dim, 0.0);
        }
        c.text(d, l, top + f.height + d.ascent, &d.fit(&ticket_detail(size, info), r - l), pal.dim, 0.0);
    }

    /// 320 x 240: the tabs, the tab's list, then the cards, the message and the hints
    pub(super) fn paint_bottom_ficelle(&self, c: &mut Canvas, b: &Bottom) -> Vec<Hit> {
        let pal = &self.ui.pal;
        let mut hits = Vec::new();
        c.fill(pal.bg);

        // tabs: the names, the current one underlined in the accent
        hline(c, 0.0, TAB_H, 320.0, pal.border);
        let mut x = 12.0;
        for &tab in &TABS {
            let on = tab == self.menu.tab;
            let f = if on { &HANKEN_SEMI_13 } else { &HANKEN_REG_13 };
            let name = tab.name();
            let w = f.width(name, 0.0).round();
            c.text(f, x, mid(f, TAB_H / 2.0), name, if on { pal.text } else { pal.dim }, 0.0);
            if on {
                c.round(x, TAB_H - 1.0, w, 2.0, 0.0, pal.accent);
            }
            hits.push(Hit { x0: x - 8.0, y0: 0.0, x1: x + w + 8.0, y1: TAB_H + 3.0, tap: Tap::Tab(tab) });
            x += w + 16.0;
        }

        let hints = match self.menu.tab {
            Tab::Recent => self.recent_hints(b),
            Tab::Browse => self.browse_hints(),
            Tab::Settings => {
                vec![(Button::A, self.settings_action()), (Button::Dpad, "")]
            }
        };
        let bottom = self.hint_bar(c, b, &hints);
        match self.menu.tab {
            Tab::Recent => self.recent_ficelle(c, bottom, &mut hits),
            Tab::Browse => self.browse_ficelle(c, bottom, &mut hits),
            Tab::Settings => self.settings_ficelle(c, bottom, &mut hits),
        }
        hits
    }

    /// the hints along the bottom, over a line, then (going up) the message and
    /// the cards; returns how far down the list can go
    fn hint_bar(&self, c: &mut Canvas, b: &Bottom, hints: &[(Button, &str)]) -> f32 {
        let pal = &self.ui.pal;
        hline(c, 0.0, HINT_TOP, 320.0, pal.border);
        let cy = HINT_TOP + 1.0 + (240.0 - HINT_TOP - 1.0) / 2.0;
        let f = &HANKEN_REG_11;
        let mut x = 12.0;
        for (k, &(button_, label)) in hints.iter().enumerate() {
            let main = k == 0;
            x += button(c, x, cy, button_, main, pal);
            if !label.is_empty() {
                x = c.text(f, x + 5.0, mid(f, cy), label, if main { pal.text } else { pal.dim }, 0.0);
            }
            x += 12.0;
        }
        // L and R switch the tabs, on the right
        let tw = f.width("tabs", 0.0);
        let mut rx = 308.0 - tw;
        c.text(f, rx, mid(f, cy), "tabs", pal.dim, 0.0);
        rx -= 5.0 + 15.0;
        letter_button(c, rx, cy, 'R', pal);
        rx -= 3.0 + 15.0;
        letter_button(c, rx, cy, 'L', pal);

        let mut bottom = HINT_TOP - 4.0;
        if b.show_msg {
            let m = &HANKEN_SEMI_12;
            bottom -= m.height;
            let color = if crate::ui::good_news(&self.cia_msg) { GOOD } else { BAD };
            c.text(m, 12.0, bottom + m.ascent, &m.fit(&self.cia_msg, 296.0), color, 0.0);
            bottom -= 4.0;
        }
        for card in self.stack_cards() {
            bottom -= card_height();
            draw_card(c, L, bottom, R - L, &card, pal);
            bottom -= 4.0;
        }
        bottom
    }

    /// the line under the tabs: what the list is, and a count or the free space
    fn section(&self, c: &mut Canvas, what: &str, count: &str) -> f32 {
        let pal = &self.ui.pal;
        let s = &HANKEN_REG_11;
        let y = TAB_H + 1.0 + 7.0;
        let cw = s.width(count, 0.0);
        c.text(s, 12.0, y + s.ascent, &s.fit_start(what, 296.0 - cw - 10.0), pal.dim, 0.0);
        c.text_right(s, 308.0, y + s.ascent, count, pal.dim);
        y + s.height + 3.0
    }

    /// a list row: the icon, the name and the size; returns the row's background
    fn list_row(&self, c: &mut Canvas, y: f32, sel: bool, glyph: Glyph, name: &str, size: &str, quiet: bool) -> Color {
        let pal = &self.ui.pal;
        let h = 26.0;
        let bg = if sel { pal.selected } else { pal.bg };
        if sel {
            c.round(L, y, R - L, h, 2.0, bg);
        }
        let cy = y + h / 2.0;
        draw_glyph(c, L + 6.0, (cy - 7.0).round(), glyph, if sel { pal.accent } else { pal.dim }, bg);
        let (f, s) = (if sel { &HANKEN_SEMI_13 } else { &HANKEN_REG_13 }, &HANKEN_REG_11);
        let r = R - 6.0;
        let l = L + 6.0 + 14.0 + 8.0;
        let sw = s.width(size, 0.0);
        c.text(f, l, mid(f, cy), &f.fit(name, r - l - sw - 10.0), if quiet && !sel { pal.dim } else { pal.text }, 0.0);
        c.text_right(s, r, mid(s, cy), size, pal.dim);
        bg
    }

    /// a quiet line in the middle of an empty list
    fn empty_ficelle(&self, c: &mut Canvas, top: f32, bottom: f32, line: &str) {
        let f = &HANKEN_REG_13;
        let line = f.fit(line, 296.0);
        c.text(f, ((320.0 - f.width(&line, 0.0)) / 2.0).round(), mid(f, (top + bottom) / 2.0), &line, self.ui.pal.dim, 0.0);
    }

    fn recent_ficelle(&self, c: &mut Canvas, bottom: f32, hits: &mut Vec<Hit>) {
        let top = self.section(c, "CIAs to install", &self.free_text());
        if let Some(line) = self.recent_empty() {
            self.empty_ficelle(c, top, bottom, line);
            return;
        }
        let (rows, first) = window(top, bottom, 26.0, 0.0, self.cia_sel);
        let mut y = top;
        for (i, cia) in self.cias.iter().enumerate().skip(first).take(rows) {
            self.list_row(c, y, i == self.cia_sel, Glyph::Package, base_name(&cia.path), &format_size(cia.size), false);
            hits.push(Hit { x0: L, y0: y, x1: R, y1: y + 26.0, tap: Tap::Item(i) });
            y += 26.0;
        }
    }

    fn browse_ficelle(&self, c: &mut Canvas, bottom: f32, hits: &mut Vec<Hit>) {
        let br = &self.menu.browse;
        let top = self.section(c, &br.dir, &self.browse_count());
        if let Some(line) = self.browse_empty() {
            self.empty_ficelle(c, top, bottom, line);
            return;
        }
        let (rows, first) = window(top, bottom, 26.0, 0.0, br.sel);
        let mut y = top;
        for (i, e) in br.entries.iter().enumerate().skip(first).take(rows) {
            let glyph = if e.dir {
                Glyph::Folder
            } else if is_cia(&e.name) {
                Glyph::Package
            } else if is_image(&e.name) {
                Glyph::Image
            } else {
                Glyph::File
            };
            let size = if e.dir { String::new() } else { format_size(e.size) };
            self.list_row(c, y, i == br.sel, glyph, &e.name, &size, !e.dir && !is_cia(&e.name));
            hits.push(Hit { x0: L, y0: y, x1: R, y1: y + 26.0, tap: Tap::Item(i) });
            y += 26.0;
        }
    }

    fn settings_ficelle(&self, c: &mut Canvas, bottom: f32, hits: &mut Vec<Hit>) {
        let pal = &self.ui.pal;
        let h = 19.0;
        let top = TAB_H + 4.0;
        let (rows, first) = window(top, bottom, h, 0.0, self.menu.row);
        let mut y = top;
        for (i, &row) in ROWS.iter().enumerate().skip(first).take(rows) {
            let sel = i == self.menu.row;
            let bg = if sel { pal.selected } else { pal.bg };
            if sel {
                c.round(L, y, R - L, h, 2.0, bg);
            }
            let f = if sel { &HANKEN_SEMI_12 } else { &HANKEN_REG_12 };
            let v = &HANKEN_REG_12;
            let cy = y + h / 2.0;
            let r = R - 6.0;
            c.text(f, L + 6.0, mid(f, cy), row_label(row), pal.text, 0.0);
            let ink = if sel { pal.text } else { pal.dim };
            let value = |c: &mut Canvas, text: &str| c.text_right(v, r, mid(v, cy), text, ink);
            match self.row_value(row) {
                RowValue::Switch(on) => switch(c, r, cy, on, bg, pal),
                RowValue::Text(text) => value(c, &text),
                RowValue::Colour(name, swatch) => {
                    let w = v.width(name, 0.0);
                    value(c, name);
                    c.round(r - w - 8.0 - 8.0, (cy - 4.0).round(), 8.0, 8.0, 2.0, swatch);
                }
                RowValue::Update(on, note) => {
                    switch(c, r, cy, on, bg, pal);
                    let r = r - 22.0 - 8.0;
                    let k = &HANKEN_SEMI_12;
                    match note {
                        Some(UpdateNote::Out(tag)) => c.text_right(k, r, mid(k, cy), &format!("{tag} is out"), pal.accent),
                        Some(UpdateNote::Restart) => c.text_right(v, r, mid(v, cy), "restart Cement", ink),
                        Some(UpdateNote::Updating) => c.text_right(v, r, mid(v, cy), "updating...", ink),
                        None => {}
                    }
                }
            }
            hits.push(Hit { x0: L, y0: y, x1: R, y1: y + h, tap: Tap::Item(i) });
            y += h;
        }
    }
}

/// an outlined round button with a letter (L and R)
fn letter_button(c: &mut Canvas, x: f32, cy: f32, ch: char, pal: &Palette) {
    let cx = x + 7.5;
    c.circle(cx, cy, 7.5, strong(pal));
    c.circle(cx, cy, 6.5, pal.bg);
    letter(c, cx, cy, ch, pal.dim);
}
