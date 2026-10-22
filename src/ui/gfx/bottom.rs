// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: GPL-3.0-or-later
// The bottom screen: the tab bar (L and R), then Recent, Browse or Settings,
// with the hints for that tab at the bottom. Drawing also records where each
// tab, row and swatch is, so a touch can be matched to it (Ui::hit).

use super::canvas::{Canvas, Color};
use super::font::*;
use super::{BAD, Button, CardText, GLYPH, GOOD, WHITE, bite_dot, card_height, draw_card, format_big, hint};
use crate::menu::{ROWS, Row, TABS, Tab, Tap};
use crate::settings::{LOOKS, Look};
use crate::picture::is_image;
use crate::ui::Bottom;
use crate::ui::palette::Palette;
use crate::util::{base_name, format_size, is_cia};
use crate::Server;

/// a spot of the screen and what touching it means
pub struct Hit {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub tap: Tap,
}

const LEFT: f32 = 13.0;
const RIGHT: f32 = 307.0;
const CONTENT: f32 = 36.0; // below the tab bar

/// a small folder: a body with a tab on its top left
fn folder(c: &mut Canvas, x: f32, y: f32, color: Color) {
    c.round(x, y, 6.0, 4.0, 1.5, color);
    c.round(x, y + 2.0, 13.0, 9.0, 2.0, color);
}

/// an on/off switch whose right edge is at `right`
fn toggle(c: &mut Canvas, right: f32, cy: f32, on: bool, pal: &Palette) {
    let (w, h) = (26.0, 14.0);
    let (x, y) = (right - w, (cy - h / 2.0).round());
    c.pill(x, y, w, h, if on { pal.accent } else { pal.track });
    let kx = if on { x + w - 7.0 } else { x + 7.0 };
    c.circle(kx, y + 7.0, 5.0, if on { WHITE } else { pal.dim });
}

/// a list row: the selected one has a border and is 2 px taller
fn row_box(c: &mut Canvas, y: f32, h: f32, sel: bool, pal: &Palette) {
    if sel {
        c.card(LEFT, y, RIGHT - LEFT, h, 8.0, pal.selected, pal.accent);
    } else {
        c.round(LEFT, y, RIGHT - LEFT, h, 8.0, pal.card);
    }
}

/// a centred line in a card, for empty lists
fn empty(c: &mut Canvas, top: f32, bottom: f32, line: &str, pal: &Palette) {
    let h = (bottom - top).min(64.0);
    c.round(LEFT, top, RIGHT - LEFT, h, 9.0, pal.card);
    let f = &NUNITO_XBOLD_13;
    let mid = (LEFT + RIGHT) / 2.0;
    c.text(f, mid - f.width(line, 0.0) / 2.0, top + (h - f.height) / 2.0 + f.ascent, &f.fit(line, RIGHT - LEFT - 16.0), pal.text, 0.0);
}

pub(super) enum RowValue {
    Switch(bool),
    Text(String),
}

pub(super) fn row_label(row: Row) -> &'static str {
    match row {
        Row::Look => "Look",
        Row::Login => "FTP login",
        Row::Ftp => "FTP server",
        Row::CiaInfo => "CIA details on the top screen",
        Row::Protect => "Protect important 3DS files",
        Row::ScreenIdle => "Screens off when idle",
        Row::AutoOff => "Turn the 3DS off when idle",
    }
}

/// a look's line on the first launch
pub(super) fn look_line(look: Look) -> &'static str {
    match look {
        Look::Cement => "Graphite and Miku teal, round and playful",
        Look::Ink => "Black and white, plain type",
    }
}

/// how many rows of height `h` (plus `gap`) fit between top and bottom, and
/// the first one to show so that `sel` is visible
pub(super) fn window(top: f32, bottom: f32, h: f32, gap: f32, sel: usize) -> (usize, usize) {
    let rows = (((bottom - top + gap - 2.0) / (h + gap)).floor() as usize).max(1);
    (rows, if sel >= rows { sel - rows + 1 } else { 0 })
}

impl Server {
    /// 320 x 240; returns where the tabs, rows and swatches are
    pub(super) fn paint_bottom(&self, c: &mut Canvas, b: &Bottom) -> Vec<Hit> {
        if self.look() == Look::Ink {
            return self.paint_bottom_ink(c, b);
        }
        let pal = &self.ui.pal;
        let mut hits = Vec::new();
        c.fill(pal.bg);
        if let Some(at) = self.menu.setup {
            self.paint_setup(c, b, at, &mut hits);
            return hits;
        }
        self.tab_bar(c, &mut hits);
        match self.menu.tab {
            Tab::Recent => self.paint_recent(c, b, &mut hits),
            Tab::Browse => self.paint_browse(c, b, &mut hits),
            Tab::Settings => self.paint_settings(c, b, &mut hits),
        }
        hits
    }

    fn tab_bar(&self, c: &mut Canvas, hits: &mut Vec<Hit>) {
        let pal = &self.ui.pal;
        let (y, h) = (6.0, 24.0);
        let gy = y + ((h - GLYPH) / 2.0).round();
        c.icon(&BTN_L, 6.0, gy, |_| pal.dim);
        c.icon(&BTN_R, 320.0 - 6.0 - BTN_R.size as f32, gy, |_| pal.dim);
        let (x0, x1, gap) = (26.0f32, 294.0, 4.0);
        let w = ((x1 - x0 - 2.0 * gap) / 3.0).floor();
        let f = &NUNITO_XBOLD_12;
        for (i, &tab) in TABS.iter().enumerate() {
            let x = x0 + i as f32 * (w + gap);
            let on = tab == self.menu.tab;
            c.pill(x, y, w, h, if on { pal.accent } else { pal.track });
            let name = tab.name();
            let tx = x + ((w - f.width(name, 0.0)) / 2.0).round();
            c.text(f, tx, y + ((h - f.height) / 2.0).round() + f.ascent, name, if on { pal.on_accent } else { pal.soft }, 0.0);
            hits.push(Hit { x0: x, y0: 0.0, x1: x + w, y1: y + h + 3.0, tap: Tap::Tab(tab) });
        }
    }

    /// the hint pills, then (going up) the message and the install card;
    /// returns how far up the list can go
    fn bottom_stack(&self, c: &mut Canvas, b: &Bottom, hints: &[(Button, &str)]) -> f32 {
        let pal = &self.ui.pal;
        let f = &NUNITO_XBOLD_11;
        let hy = 230.0 - (f.height + 6.0);
        let mut x = LEFT;
        for (k, &(button, label)) in hints.iter().enumerate() {
            let (bg, fg) = if k == 0 { (pal.accent, pal.on_accent) } else { (pal.track, pal.soft) };
            x = hint(c, x, hy, button, label, bg, fg, pal) + 5.0;
        }
        let mut bottom = hy - 5.0;
        if b.show_msg {
            let f = &NUNITO_XBOLD_12;
            bottom -= f.height;
            let color = if crate::ui::good_news(&self.cia_msg) { GOOD } else { BAD };
            c.text(f, LEFT, bottom + f.ascent, &f.fit(&self.cia_msg, RIGHT - LEFT), color, 0.0);
            bottom -= 4.0;
        }
        if let Some(card) = self.install_card() {
            bottom -= card_height();
            draw_card(c, LEFT, bottom, RIGHT - LEFT, &card, pal);
            bottom -= 5.0;
        }
        bottom
    }

    /// the install going on, outside Settings
    pub(super) fn install_card(&self) -> Option<CardText> {
        let inst = self.inst.as_ref().filter(|_| self.menu.tab != Tab::Settings)?;
        let done = *inst.shared.done.lock().unwrap();
        let pct = if inst.total != 0 { done * 100 / inst.total } else { 0 };
        let preparing = inst.shared.preparing.load(std::sync::atomic::Ordering::Relaxed);
        Some(CardText {
            title: format!("Installing {}", base_name(&inst.path)),
            value: if preparing { String::new() } else { format!("{pct}%") },
            frac: (inst.total != 0 && !preparing).then(|| done as f32 / inst.total as f32),
            detail: if preparing {
                "Preparing... removing the old copy".into()
            } else {
                format!("{} of {}", format_size(done), format_size(inst.total))
            },
            right: String::new(),
        })
    }

    /// a Settings row's value: a switch, or a text
    pub(super) fn row_value(&self, row: Row) -> RowValue {
        match row {
            Row::Look => RowValue::Text(self.prefs.look.name().to_string()),
            Row::Ftp => RowValue::Switch(self.prefs.ftp),
            Row::CiaInfo => RowValue::Switch(self.prefs.cia_info),
            Row::Protect => RowValue::Switch(self.prefs.protect),
            Row::ScreenIdle => RowValue::Switch(self.prefs.screen_idle),
            Row::Login => RowValue::Text(self.login.as_ref().map_or("none", |l| l.0.as_str()).to_string()),
            Row::AutoOff => RowValue::Text(match self.prefs.auto_off {
                0 => "never".to_string(),
                m => format!("after {m} min"),
            }),
        }
    }

    /// first launch: the two looks to pick from
    fn paint_setup(&self, c: &mut Canvas, b: &Bottom, at: usize, hits: &mut Vec<Hit>) {
        let pal = &self.ui.pal;
        let bottom = self.bottom_stack(c, b, &[(Button::A, "use this look"), (Button::Dpad, "")]);
        let (t, s) = (&NUNITO_XBOLD_13, &NUNITO_BOLD_11);
        let mut y = 14.0;
        c.text(t, LEFT, y + t.ascent, "Pick a look", pal.text, 0.0);
        y += t.height;
        c.text(s, LEFT, y + s.ascent, "You can change it later in Settings", pal.dim, 0.0);
        y += s.height + 10.0;
        let h = 40.0;
        for (i, look) in LOOKS.iter().enumerate() {
            if y + h > bottom {
                break;
            }
            let sel = i == at;
            row_box(c, y, h, sel, pal);
            let f = if sel { &NUNITO_XBOLD_13 } else { &NUNITO_BOLD_13 };
            let top = (y + (h - f.height - s.height) / 2.0).round();
            c.text(f, LEFT + 10.0, top + f.ascent, look.name(), pal.text, 0.0);
            c.text(s, LEFT + 10.0, top + f.height + s.ascent, look_line(*look), if sel { pal.soft } else { pal.dim }, 0.0);
            hits.push(Hit { x0: LEFT, y0: y, x1: RIGHT, y1: y + h, tap: Tap::Item(i) });
            y += h + 5.0;
        }
    }

    /// the CIAs sent since Cement started, or found in the usual folders
    fn paint_recent(&self, c: &mut Canvas, b: &Bottom, hits: &mut Vec<Hit>) {
        let pal = &self.ui.pal;
        let s = &NUNITO_BOLD_11;
        let free = self.ui.sd_free.map(|v| format!("{} free", format_big(v))).unwrap_or_default();
        // the section marker, and the free space
        bite_dot(c, LEFT, CONTENT + ((s.height - 12.0) / 2.0).round(), pal);
        c.text_right(s, RIGHT, CONTENT + s.ascent, &free, pal.dim);
        let top = CONTENT + s.height + 4.0;

        let first = if b.busy && !self.installing() { "waits for transfers" } else { "install" };
        let mut hints = vec![(Button::A, first), (Button::Dpad, "")];
        if !self.cias.is_empty() {
            hints.push((Button::X, "delete"));
        }
        let bottom = self.bottom_stack(c, b, &hints);
        if self.cias.is_empty() && self.inst.is_none() {
            empty(c, top, bottom, "No CIAs yet: upload one over FTP", pal);
            return;
        }
        let f = &NUNITO_XBOLD_13;
        let (h, gap) = (f.height + 10.0, 5.0);
        let (rows, first) = window(top, bottom, h, gap, self.cia_sel);
        let mut y = top;
        for (i, cia) in self.cias.iter().enumerate().skip(first).take(rows) {
            let sel = i == self.cia_sel;
            let (rh, inset) = if sel { (h + 2.0, 1.0) } else { (h, 0.0) };
            row_box(c, y, rh, sel, pal);
            let f = if sel { &NUNITO_XBOLD_13 } else { &NUNITO_BOLD_13 };
            let base = y + inset + 5.0 + f.ascent;
            let (l, r) = (LEFT + inset + 9.0, RIGHT - inset - 9.0);
            let size = format_size(cia.size);
            let name = f.fit(base_name(&cia.path), r - l - f.width(&size, 0.0) - 10.0);
            c.text(f, l, base, &name, pal.text, 0.0);
            c.text_right(f, r, base, &size, if sel { pal.soft } else { pal.dim });
            hits.push(Hit { x0: LEFT, y0: y, x1: RIGHT, y1: y + rh, tap: Tap::Item(i) });
            y += rh + gap;
        }
    }

    /// one folder of the SD card
    fn paint_browse(&self, c: &mut Canvas, b: &Bottom, hits: &mut Vec<Hit>) {
        let pal = &self.ui.pal;
        let br = &self.menu.browse;
        let s = &NUNITO_BOLD_11;
        let count = match (self.ui.viewed_size(), br.entries.len()) {
            (Some((w, h)), _) => format!("{w} x {h}"),
            (None, 1) => "1 item".to_string(),
            (None, n) => format!("{n} items"),
        };
        let cw = s.width(&count, 0.0);
        let p = &NUNITO_XBOLD_12;
        let path = p.fit_start(&br.dir, RIGHT - LEFT - 16.0 - cw - 10.0);
        bite_dot(c, LEFT, CONTENT + ((p.height - 12.0) / 2.0).round(), pal);
        c.text(p, LEFT + 16.0, CONTENT + p.ascent, &path, pal.soft, 0.0);
        c.text_right(s, RIGHT, CONTENT + (p.height - s.height) / 2.0 + s.ascent, &count, pal.dim);
        let top = CONTENT + p.height + 3.0;

        let action = match br.entries.get(br.sel) {
            Some(e) if !e.dir && is_cia(&e.name) => "install",
            Some(e) if !e.dir && is_image(&e.name) => {
                if self.menu.view { "close" } else { "view" }
            }
            _ => "open",
        };
        let mut hints = vec![(Button::A, action)];
        if self.menu.view {
            hints.push((Button::B, "close"));
        } else if br.dir != "/" {
            hints.push((Button::B, "back"));
        }
        hints.push((Button::Dpad, ""));
        if !br.entries.is_empty() {
            hints.push((Button::X, "delete"));
        }
        let bottom = self.bottom_stack(c, b, &hints);
        if br.failed {
            empty(c, top, bottom, "This folder can't be opened", pal);
            return;
        }
        if br.entries.is_empty() {
            empty(c, top, bottom, if br.loaded { "This folder is empty" } else { "Reading the folder..." }, pal);
            return;
        }
        let f = &NUNITO_BOLD_12;
        let (h, gap) = (f.height + 8.0, 4.0);
        let (rows, first) = window(top, bottom, h, gap, br.sel);
        let mut y = top;
        for (i, e) in br.entries.iter().enumerate().skip(first).take(rows) {
            let sel = i == br.sel;
            let (rh, inset) = if sel { (h + 2.0, 1.0) } else { (h, 0.0) };
            row_box(c, y, rh, sel, pal);
            let f = if sel { &NUNITO_XBOLD_12 } else { &NUNITO_BOLD_12 };
            let base = y + inset + 4.0 + f.ascent;
            let (mut l, r) = (LEFT + inset + 9.0, RIGHT - inset - 9.0);
            let size = if e.dir { String::new() } else { format_size(e.size) };
            if e.dir {
                folder(c, l, (y + (rh - 11.0) / 2.0).round(), pal.highlight);
                l += 18.0;
            }
            let ink = if e.dir || is_cia(&e.name) { pal.text } else { pal.dim };
            let name = f.fit(&e.name, r - l - f.width(&size, 0.0) - 10.0);
            c.text(f, l, base, &name, ink, 0.0);
            c.text_right(f, r, base, &size, if sel { pal.soft } else { pal.dim });
            hits.push(Hit { x0: LEFT, y0: y, x1: RIGHT, y1: y + rh, tap: Tap::Item(i) });
            y += rh + gap;
        }
    }

    /// the login, the FTP server, the CIA details, the screens and the console turning off
    fn paint_settings(&self, c: &mut Canvas, b: &Bottom, hits: &mut Vec<Hit>) {
        let pal = &self.ui.pal;
        let bottom = self.bottom_stack(c, b, &[(Button::A, "change"), (Button::Dpad, "")]);
        let (h, gap) = (21.0, 2.0);
        let (rows, first) = window(CONTENT, bottom, h, gap, self.menu.row);
        let mut y = CONTENT;
        for (i, &row) in ROWS.iter().enumerate().skip(first).take(rows) {
            let sel = i == self.menu.row;
            row_box(c, y, h, sel, pal);
            let f = if sel { &NUNITO_XBOLD_12 } else { &NUNITO_BOLD_12 };
            let base = y + ((h - f.height) / 2.0).round() + f.ascent;
            let (l, r) = (LEFT + 9.0, RIGHT - 9.0);
            let cy = y + h / 2.0;
            c.text(f, l, base, row_label(row), pal.text, 0.0);
            match self.row_value(row) {
                RowValue::Switch(on) => toggle(c, r, cy, on, pal),
                RowValue::Text(text) => c.text_right(f, r, base, &text, if sel { pal.soft } else { pal.dim }),
            }
            hits.push(Hit { x0: LEFT, y0: y, x1: RIGHT, y1: y + h, tap: Tap::Item(i) });
            y += h + gap;
        }
    }
}
