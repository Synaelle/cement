// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The Ink look: black and white, plain type (Ink Sans, IBM Plex Mono for the
// address), hairlines instead of cards, the selection drawn inverted. Same
// content as the Cement look (mod.rs and bottom.rs work it out), drawn apart.

use super::bottom::{Hit, RowValue, look_line, row_label, window};
use super::canvas::{Canvas, Color};
use super::font::*;
use super::{CardText, ICON, format_big, help, install_kind, ticket_text, wrap};
use crate::menu::{ROWS, Row, TABS, Tab, Tap};
use crate::picture::is_image;
use crate::settings::LOOKS;
use crate::ui::{Bottom, Top};
use crate::util::{base_name, format_size, ip_text, is_cia};
use crate::{PROTOCOL, Server, VERSION, sys};

pub const BG: Color = 0x131313;
const TEXT: Color = 0xececec;
const DIM: Color = 0x9b9b9b;
const LINE: Color = 0x2b2b2b;

/// a 1 px rule across
fn rule(c: &mut Canvas, x0: f32, x1: f32, y: f32) {
    c.round(x0, y, x1 - x0, 1.0, 0.0, LINE);
}

/// title and value on a line, a 2 px bar, detail and time under it; the
/// whole block is `progress_height()` tall
fn progress(c: &mut Canvas, l: f32, r: f32, top: f32, card: &CardText) {
    let (f, d) = (&INK_MED_12, &INK_REG_11);
    let vw = f.width(&card.value, 0.0);
    c.text(f, l, top + f.ascent, &f.fit(&card.title, r - l - vw - 10.0), TEXT, 0.0);
    c.text_right(f, r, top + f.ascent, &card.value, TEXT);
    let bar = top + f.height + 5.0;
    c.round(l, bar, r - l, 2.0, 0.0, LINE);
    if let Some(frac) = card.frac {
        let w = ((r - l) * frac.clamp(0.0, 1.0)).round();
        if w > 0.0 {
            c.round(l, bar, w, 2.0, 0.0, TEXT);
        }
    }
    let dy = bar + 2.0 + 5.0;
    let rw = d.width(&card.right, 0.0);
    c.text(d, l, dy + d.ascent, &d.fit(&card.detail, r - l - rw - 10.0), DIM, 0.0);
    c.text_right(d, r, dy + d.ascent, &card.right, DIM);
}

fn progress_height() -> f32 {
    INK_MED_12.height + 5.0 + 2.0 + 5.0 + INK_REG_11.height
}

/// the image viewer's message, centred, no card
pub fn paint_note(c: &mut Canvas, title: &str, detail: &str) {
    c.fill(BG);
    let (f, d) = (&INK_MED_12, &INK_REG_11);
    let h = if detail.is_empty() { f.height } else { f.height + 4.0 + d.height };
    let y = ((c.h as f32 - h) / 2.0).round();
    let title = f.fit(title, c.w as f32 - 40.0);
    c.text(f, ((c.w as f32 - f.width(&title, 0.0)) / 2.0).round(), y + f.ascent, &title, TEXT, 0.0);
    if !detail.is_empty() {
        let detail = d.fit(detail, c.w as f32 - 40.0);
        c.text(d, ((c.w as f32 - d.width(&detail, 0.0)) / 2.0).round(), y + f.height + 4.0 + d.ascent, &detail, DIM, 0.0);
    }
}

/// "A install   X delete": each key bright, what it does dim
fn hints(c: &mut Canvas, x: f32, base: f32, hints: &[(&str, &str)]) {
    let (k, l) = (&INK_SEMI_12, &INK_REG_12);
    let mut x = x;
    for (key, what) in hints {
        x = c.text(k, x, base, key, TEXT, 0.0) + 5.0;
        x = c.text(l, x, base, what, DIM, 0.0) + 14.0;
    }
}

/// a list row: inverted when selected, else a rule under it; the name on the
/// left (dim when `faint`), the value on the right
fn row(c: &mut Canvas, l: f32, r: f32, y: f32, h: f32, sel: bool, name: &str, value: &str, faint: bool) {
    let f = if sel { &INK_SEMI_12 } else { &INK_REG_12 };
    let base = y + ((h - f.height) / 2.0).round() + f.ascent;
    let (ink, dim) = if sel {
        c.round(l, y, r - l, h, 2.0, TEXT);
        (BG, BG)
    } else {
        rule(c, l, r, y + h - 1.0);
        (if faint { DIM } else { TEXT }, DIM)
    };
    let vw = f.width(value, 0.0);
    c.text(f, l + 7.0, base, &f.fit(name, r - l - 14.0 - vw - 10.0), ink, 0.0);
    c.text_right(f, r - 7.0, base, value, dim);
}

/// a line in the middle of an empty list
fn empty(c: &mut Canvas, l: f32, r: f32, top: f32, bottom: f32, line: &str) {
    let f = &INK_REG_12;
    let line = f.fit(line, r - l);
    let y = (top + ((bottom - top).min(60.0) - f.height) / 2.0).round();
    c.text(f, ((l + r - f.width(&line, 0.0)) / 2.0).round(), y + f.ascent, &line, DIM, 0.0);
}

impl Server {
    /// 400 x 240: name, version, START and battery; the address; the selected
    /// CIA or what a Settings row does; the transfer at the bottom
    pub(super) fn paint_top_ink(&self, c: &mut Canvas, t: &Top) {
        let now = sys::now_ms();
        c.fill(BG);
        let (l, r) = (17.0, 383.0);
        let (t16, m12, s11) = (&INK_SEMI_16, &INK_MED_12, &INK_REG_11);
        let mut y = 12.0;
        let base = y + t16.ascent;
        let end = c.text(t16, l, base, "Cement", TEXT, 0.0);
        let label = Self::battery_label(t);
        let bw = m12.width(&label, 0.0);
        let mut x = if t.low {
            // low: the only thing drawn inverted up here
            c.round(r - bw - 8.0, base - m12.ascent - 2.0, bw + 8.0, m12.height + 3.0, 2.0, TEXT);
            c.text(m12, r - bw - 4.0, base, &label, BG, 0.0);
            r - bw - 8.0
        } else {
            c.text(m12, r - bw, base, &label, TEXT, 0.0);
            r - bw
        };
        x -= 8.0 + s11.width("·", 0.0);
        c.text(s11, x, base, "·", DIM, 0.0);
        x -= 8.0 + s11.width("START exit", 0.0);
        c.text(s11, x, base, "START exit", DIM, 0.0);
        let version = s11.fit(&format!("{VERSION} · protocol {PROTOCOL}"), x - 10.0 - end - 8.0);
        c.text(s11, end + 8.0, base, &version, DIM, 0.0);
        y += t16.height + 6.0;
        rule(c, l, r, y);
        y += 7.0;

        // the address
        c.text(s11, l, y + s11.ascent, "Connect to", DIM, 0.0);
        c.text_right(s11, r, y + s11.ascent, &s11.fit(&self.login_text(), 170.0), DIM);
        y += s11.height;
        let mono = &MONO_MED_22;
        let ip = if self.net.ip != 0 { ip_text(self.net.ip) } else { "-".into() };
        c.text(mono, l, y + mono.ascent, &format!("{ip}:{}", sys::ports().0), TEXT, 0.0);
        y += mono.height + 5.0;
        rule(c, l, r, y);
        y += 8.0;

        // under it: a low battery warning, what the selected row does, or the selected CIA
        let ticket = self.prefs.cia_info.then_some(self.ui.info.as_ref()).flatten();
        let help_row = if self.menu.setup.is_some() {
            Some(Row::Look)
        } else {
            (self.menu.tab == Tab::Settings).then(|| ROWS[self.menu.row])
        };
        let slot_end = if t.low {
            for line in ["Battery low: no new transfers", "or installs until you charge"] {
                c.text(m12, l, y + m12.ascent, line, TEXT, 0.0);
                y += m12.height;
            }
            Some(y)
        } else if let Some(row) = help_row {
            let f = &INK_REG_12;
            let mut lines = wrap(f, help(row), r - l);
            if lines.len() > 2 {
                let rest = lines[1..].join(" ");
                lines = vec![lines[0].clone(), f.fit(&rest, r - l)];
            }
            for line in &lines {
                c.text(f, l, y + f.ascent, line, TEXT, 0.0);
                y += f.height;
            }
            Some(y)
        } else if let Some((path, size, info)) = ticket {
            let iy = y;
            match info.as_ref().and_then(|i| i.icon.as_ref()) {
                Some(px) => c.image(l, iy, ICON, px, 3.0),
                None => c.round(l, iy, ICON as f32, ICON as f32, 3.0, LINE),
            }
            let tl = l + ICON as f32 + 10.0;
            let mut tr = r;
            if let Some(info) = info {
                let kind = install_kind(info);
                let kw = m12.width(kind, 0.0);
                let kb = iy + (ICON as f32 - m12.height) / 2.0 + m12.ascent;
                c.text(m12, r - kw, kb.round(), kind, TEXT, 0.0);
                c.round(r - kw, kb.round() + 3.0, kw, 1.0, 0.0, TEXT); // underlined
                tr -= kw + 12.0;
            }
            let (title, publisher, detail) = ticket_text(path, *size, info.as_ref());
            let f = &INK_REG_12;
            let top = (iy + (ICON as f32 - m12.height - s11.height) / 2.0).round();
            let end = c.text(m12, tl, top + m12.ascent, &m12.fit(title, tr - tl), TEXT, 0.0);
            if !publisher.is_empty() && end + 8.0 < tr - 20.0 {
                c.text(f, end + 6.0, top + m12.ascent, &f.fit(&format!("by {publisher}"), tr - end - 6.0), DIM, 0.0);
            }
            c.text(s11, tl, top + m12.height + s11.ascent, &s11.fit(&detail, tr - tl), DIM, 0.0);
            Some(iy + ICON as f32)
        } else {
            None
        };
        if let Some(end) = slot_end {
            rule(c, l, r, end + 7.0);
        }

        // the transfer, at the bottom
        let card = self.transfer_card(t, now);
        progress(c, l, r, 240.0 - 13.0 - progress_height(), &card);
    }

    /// 320 x 240: the tabs as words, the list, the hints as text at the bottom
    pub(super) fn paint_bottom_ink(&self, c: &mut Canvas, b: &Bottom) -> Vec<Hit> {
        c.fill(BG);
        let mut hits = Vec::new();
        let (l, r) = (13.0, 307.0);
        let (m12, s11) = (&INK_MED_12, &INK_REG_11);
        let foot = 240.0 - 22.0;
        rule(c, l, r, foot);
        let hint_base = foot + 5.0 + INK_SEMI_12.ascent;

        if let Some(at) = self.menu.setup {
            let t = &INK_SEMI_12;
            let mut y = 14.0;
            c.text(t, l, y + t.ascent, "Pick a look", TEXT, 0.0);
            y += t.height;
            c.text(s11, l, y + s11.ascent, "You can change it later in Settings", DIM, 0.0);
            y += s11.height + 10.0;
            rule(c, l, r, y - 1.0);
            let h = 36.0;
            for (i, look) in LOOKS.iter().enumerate() {
                let sel = i == at;
                let (ink, dim) = if sel {
                    c.round(l, y, r - l, h, 2.0, TEXT);
                    (BG, BG)
                } else {
                    rule(c, l, r, y + h - 1.0);
                    (TEXT, DIM)
                };
                let f = if sel { &INK_SEMI_12 } else { &INK_REG_12 };
                let top = (y + (h - f.height - s11.height) / 2.0).round();
                c.text(f, l + 7.0, top + f.ascent, look.name(), ink, 0.0);
                c.text(s11, l + 7.0, top + f.height + s11.ascent, look_line(*look), dim, 0.0);
                hits.push(Hit { x0: l, y0: y, x1: r, y1: y + h, tap: Tap::Item(i) });
                y += h;
            }
            hints(c, l, hint_base, &[("A", "use this look")]);
            return hits;
        }

        // the tabs: words, the current one bright and underlined
        let y = 8.0;
        let base = y + m12.ascent;
        let mut x = l;
        c.text(s11, x, base, "L", DIM, 0.0);
        x += s11.width("L", 0.0) + 14.0;
        for &tab in TABS.iter() {
            let on = tab == self.menu.tab;
            let f = if on { &INK_SEMI_12 } else { m12 };
            let w = f.width(tab.name(), 0.0);
            c.text(f, x, base, tab.name(), if on { TEXT } else { DIM }, 0.0);
            if on {
                c.round(x, base + 5.0, w, 2.0, 0.0, TEXT);
            }
            hits.push(Hit { x0: x - 7.0, y0: 0.0, x1: x + w + 7.0, y1: 31.0, tap: Tap::Tab(tab) });
            x += w + 18.0;
        }
        c.text_right(s11, r, base, "R", DIM);
        rule(c, l, r, 30.0);
        let top = 36.0;

        // above the hints: the message, then the install
        let mut bottom = foot - 4.0;
        if b.show_msg {
            let good = crate::ui::good_news(&self.cia_msg);
            let f = if good { &INK_REG_12 } else { &INK_SEMI_12 };
            bottom -= f.height;
            c.text(f, l, bottom + f.ascent, &f.fit(&self.cia_msg, r - l), TEXT, 0.0);
            bottom -= 4.0;
        }
        if let Some(card) = self.install_card() {
            bottom -= progress_height();
            progress(c, l, r, bottom, &card);
            bottom -= 6.0;
            rule(c, l, r, bottom);
            bottom -= 4.0;
        }

        match self.menu.tab {
            Tab::Recent => {
                let free = self.ui.sd_free.map(|v| format!("{} free", format_big(v))).unwrap_or_default();
                c.text_right(s11, r, hint_base, &free, DIM);
                let first = if b.busy && !self.installing() { "waits for transfers" } else { "install" };
                let mut keys = vec![("A", first)];
                if !self.cias.is_empty() {
                    keys.push(("X", "delete"));
                }
                hints(c, l, hint_base, &keys);
                if self.cias.is_empty() && self.inst.is_none() {
                    empty(c, l, r, top, bottom, "No CIAs yet: upload one over FTP");
                    return hits;
                }
                let h = 21.0;
                let (rows, first) = window(top, bottom, h, 0.0, self.cia_sel);
                let mut y = top;
                for (i, cia) in self.cias.iter().enumerate().skip(first).take(rows) {
                    row(c, l, r, y, h, i == self.cia_sel, base_name(&cia.path), &format_size(cia.size), false);
                    hits.push(Hit { x0: l, y0: y, x1: r, y1: y + h, tap: Tap::Item(i) });
                    y += h;
                }
            }
            Tab::Browse => {
                let br = &self.menu.browse;
                let count = match (self.ui.viewed_size(), br.entries.len()) {
                    (Some((w, h)), _) => format!("{w} x {h}"),
                    (None, 1) => "1 item".to_string(),
                    (None, n) => format!("{n} items"),
                };
                c.text_right(s11, r, hint_base, &count, DIM);
                let sel = br.entries.get(br.sel);
                let action = match sel {
                    Some(e) if !e.dir && is_cia(&e.name) => "install",
                    Some(e) if !e.dir && is_image(&e.name) => {
                        if self.menu.view { "close" } else { "view" }
                    }
                    _ => "open",
                };
                let mut keys = vec![("A", action)];
                if self.menu.view {
                    keys.push(("B", "close"));
                } else if br.dir != "/" {
                    keys.push(("B", "back"));
                }
                if sel.is_some() {
                    keys.push(("X", "delete"));
                }
                hints(c, l, hint_base, &keys);
                c.text(s11, l, top + s11.ascent, &s11.fit_start(&br.dir, r - l), DIM, 0.0);
                let top = top + s11.height + 4.0;
                if br.failed {
                    empty(c, l, r, top, bottom, "This folder can't be opened");
                    return hits;
                }
                if br.entries.is_empty() {
                    empty(c, l, r, top, bottom, if br.loaded { "This folder is empty" } else { "Reading the folder..." });
                    return hits;
                }
                let h = 21.0;
                let (rows, first) = window(top, bottom, h, 0.0, br.sel);
                let mut y = top;
                for (i, e) in br.entries.iter().enumerate().skip(first).take(rows) {
                    let (name, size) = if e.dir { (format!("{}/", e.name), String::new()) } else { (e.name.clone(), format_size(e.size)) };
                    let faint = !e.dir && !is_cia(&e.name) && !is_image(&e.name);
                    row(c, l, r, y, h, i == br.sel, &name, &size, faint);
                    hits.push(Hit { x0: l, y0: y, x1: r, y1: y + h, tap: Tap::Item(i) });
                    y += h;
                }
            }
            Tab::Settings => {
                hints(c, l, hint_base, &[("A", "change")]);
                let h = 21.0;
                let (rows, first) = window(top, bottom, h, 0.0, self.menu.row);
                let mut y = top;
                for (i, &r_) in ROWS.iter().enumerate().skip(first).take(rows) {
                    let value = match self.row_value(r_) {
                        RowValue::Switch(on) => if on { "on" } else { "off" }.to_string(),
                        RowValue::Text(text) => text,
                    };
                    row(c, l, r, y, h, i == self.menu.row, row_label(r_), &value, false);
                    hits.push(Hit { x0: l, y0: y, x1: r, y1: y + h, tap: Tap::Item(i) });
                    y += h;
                }
            }
        }
        hits
    }
}
