//! The GIF grid: masonry layout, keyboard selection, tiles and their right-click menu.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, LayerId, Order, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

use crate::{
    app::{Action, App, View, gif_count},
    db::{Gif, GifId},
    media::Span,
    urls,
};

use super::{
    Dir,
    icons::{Icon, paint_icon},
    menu::{context_menu, menu_check, menu_field, menu_item, menu_label, menu_note, menu_sep},
    theme::{ACCENT, BLURPLE, FAINT, GAP, LINE, MUTED, RADIUS, RAISED, RED, SURFACE, TEXT, Tone, fade, mix, semi},
    widgets::{bottom_fade, columns, paint_cover},
};

impl App {
    pub(super) fn grid(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let n = self.list.len();
        if n == 0 {
            let msg = if !self.query.is_empty() {
                format!("Nothing matches “{}”", self.query.trim())
            } else {
                match self.view {
                    View::Collection(_) => "Empty. Right-click any GIF → Collections to add it here.".into(),
                    _ => "Nothing here yet.".into(),
                }
            };
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.32);
                let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                paint_icon(ui.painter(), Icon::Search, r.center(), 22.0, FAINT);
                ui.add_space(4.0);
                ui.label(RichText::new(msg).color(MUTED));
            });
            return;
        }

        // Leaves room for the scroll bar's gutter.
        let width = ui.available_width() - ui.spacing().scroll.allocated_width();
        let total = self.masonry(width, ui.ctx().pixels_per_point());
        let mut sa = ScrollArea::vertical().auto_shrink(false).id_salt(("grid", self.view));
        if let Some(off) = self.grid.scroll_to.take() {
            sa = sa.vertical_scroll_offset(off);
        }
        let out = sa.show_viewport(ui, |ui, vp| {
            let (area, _) = ui.allocate_exact_size(vec2(width, total), Sense::hover());
            let origin = area.min.to_vec2();
            self.set_focus(Span { top: vp.min.y, bottom: vp.max.y });
            // The next screen and a half, and half a screen back, get downloaded ahead of time.
            let (behind, ahead) = (vp.min.y - vp.height() * 0.5, vp.max.y + vp.height() * 1.5);
            for i in 0..self.grid.tiles.len() {
                let t = self.grid.tiles[i];
                if t.max.y >= vp.min.y && t.min.y <= vp.max.y {
                    self.gif_tile(ui, i, t.translate(origin), actions);
                } else if t.max.y >= behind && t.min.y <= ahead {
                    self.warm(i);
                }
            }
        });
        self.grid.scroll_off = out.state.clamped_offset().y;
        self.grid.view_h = out.inner_rect.height();
    }

    /// Discord-style masonry: equal-width columns, each GIF at its own aspect ratio, placed in
    /// the shortest column. Cheap enough (a few thousand adds) to redo every frame, so it
    /// reflows by itself when sizes arrive. Returns the total height.
    pub(super) fn masonry(&mut self, width: f32, ppp: f32) -> f32 {
        let (cols, cw) = columns(width);
        self.grid.tile_px = (cw * ppp).round() as u32;
        let mut heights = vec![0.0f32; cols];
        // A new list clears the tiles, so this also catches one laid out the same as the last.
        let old = std::mem::replace(&mut self.grid.tiles, Vec::with_capacity(self.list.len()));
        for id in &self.list {
            let ratio = self.lib.gif(*id).map_or(0.75, Gif::ratio);
            // Clamp extremes so one panorama or tall strip doesn't wreck the layout.
            let h = (cw * ratio.clamp(0.4, 1.8)).round();
            let (c, y) = heights.iter().copied().enumerate().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0, 0.0));
            self.grid.tiles.push(Rect::from_min_size(pos2(c as f32 * (cw + GAP), y), vec2(cw, h)));
            heights[c] = y + h + GAP;
        }
        if self.grid.tiles != old {
            let place = |t: &Rect| Span { top: t.min.y, bottom: t.max.y };
            self.set_places(self.list.iter().zip(&self.grid.tiles).map(|(&id, t)| (id, place(t))).collect());
        }
        (heights.into_iter().fold(0.0, f32::max) - GAP).max(0.0)
    }

    /// The tile next to `s` in a direction: same column for up/down, the neighbouring column's
    /// closest tile for left/right.
    pub(super) fn step(&self, s: usize, dir: Dir) -> Option<usize> {
        let (dx, dy) = match dir {
            Dir::Up => (0.0, -1.0),
            Dir::Down => (0.0, 1.0),
            Dir::Left => (-1.0, 0.0),
            Dir::Right => (1.0, 0.0),
        };
        let cur = *self.grid.tiles.get(s)?;
        let x = cur.min.x + dx * (cur.width() + GAP);
        self.grid
            .tiles
            .iter()
            .enumerate()
            .filter(|(_, t)| (t.min.x - x).abs() < 1.0)
            .filter(|(_, t)| dy == 0.0 || (t.center().y - cur.center().y) * dy > 0.0)
            .min_by(|a, b| {
                let d = |t: &Rect| (t.center().y - cur.center().y).abs();
                d(a.1).total_cmp(&d(b.1))
            })
            .map(|(i, _)| i)
    }

    pub(super) fn gif_tile(&mut self, ui: &mut Ui, i: usize, rect: Rect, actions: &mut Vec<Action>) {
        let id = self.list[i];
        let resp = ui.interact(rect, Id::unique(("gif", id)), Sense::click_and_drag());
        let Some(gi) = self.lib.index(id) else { return };
        if !ui.is_rect_visible(rect) {
            return;
        }
        let hovered = resp.hovered();
        let selected = self.grid.selected == Some(i);
        let picked = self.picked.contains(&id);
        // Keyed by GIF, not position, so scrolling doesn't smear the animation across tiles.
        let hover = fade(ui, Id::unique(("tile", id)), hovered);

        ui.painter().rect_filled(rect, RADIUS, SURFACE);
        let thumb = self.thumb(gi, true);
        let play = !self.prefs.hover_play || hovered || selected;
        let frame = if thumb.is_some() && play { self.anim_frame(gi, true) } else { None };
        // Fades in as it arrives; one already loaded (scrolling back) shows at once.
        let shown = ui.ctx().animate_bool_with_time(Id::unique(("shown", id)), thumb.is_some(), 0.15);
        if shown < 1.0 && !self.load_failed(gi) {
            loading_glow(ui, rect, 1.0 - shown);
        }
        let g = &self.lib.gifs()[gi];
        let p = ui.painter();
        match frame.or(thumb) {
            Some(t) => paint_cover(ui, rect, &t, Color32::WHITE.gamma_multiply(shown)),
            None => {
                let t: String = g.title.chars().take(22).collect();
                p.text(rect.center(), Align2::CENTER_CENTER, t, FontId::proportional(11.0), FAINT);
            }
        }
        if !g.in_library() && self.view != View::Imports {
            let c = rect.right_top() + vec2(-10.0, 10.0);
            p.circle_filled(c, 5.5, Color32::from_black_alpha(120));
            p.circle_filled(c, 3.5, BLURPLE);
        }
        if hover > 0.01 && !g.title.is_empty() {
            bottom_fade(p, rect, 34.0, (190.0 * hover) as u8);
            let strip =
                Rect::from_min_max(pos2(rect.left() + 8.0, rect.bottom() - 22.0), rect.right_bottom() - vec2(8.0, 0.0));
            let title: String = g.title.chars().take(48).collect();
            p.with_clip_rect(strip).text(
                strip.left_center(),
                Align2::LEFT_CENTER,
                title,
                FontId::proportional(12.0),
                Color32::WHITE.gamma_multiply(hover),
            );
        }
        if picked {
            p.rect_filled(rect, RADIUS, ACCENT.gamma_multiply(0.18));
            let c = rect.left_top() + vec2(14.0, 14.0);
            p.circle_filled(c, 9.0, ACCENT);
            paint_icon(p, Icon::Check, c, 11.0, Color32::WHITE);
        }
        if selected || picked {
            p.rect_stroke(rect, RADIUS, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
        } else if self.is_gone(id) {
            let c = RED.gamma_multiply(0.45 + 0.35 * hover);
            p.rect_stroke(rect, RADIUS, Stroke::new(1.5, c), StrokeKind::Inside);
        } else if hover > 0.01 {
            p.rect_stroke(
                rect,
                RADIUS,
                Stroke::new(1.0, Color32::from_white_alpha((45.0 * hover) as u8)),
                StrokeKind::Inside,
            );
        }

        if resp.drag_started() {
            self.grid.filing = Some(if picked { self.picked.clone() } else { vec![id] });
        }
        if resp.clicked() {
            // Shift+click starts picking; once something is picked, clicks pick too, rather than
            // paste. Ctrl+click pastes the other way, like Shift+Enter.
            let m = ui.input(|i| i.modifiers);
            let pick = m.shift || !self.picked.is_empty();
            actions.push(if pick {
                Action::Pick { id, range: m.shift }
            } else {
                Action::Send { id, invert: m.command }
            });
        }
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        if picked && self.picked.len() > 1 {
            context_menu(&resp, 220.0, |ui| self.picks_menu(ui, actions));
        } else {
            context_menu(&resp, 220.0, |ui| self.gif_menu(ui, id, gi, actions));
        }
    }

    /// The right-click menu of a picked GIF: changes all the picked ones.
    fn picks_menu(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        menu_label(ui, &format!("{} GIFs selected", self.picked.len()));
        menu_sep(ui);
        self.collections_menu(ui, self.picked.clone(), actions);
        menu_sep(ui);
        if menu_item(ui, Icon::Close, "Clear selection", Tone::Plain).clicked() {
            actions.push(Action::ClearPicks);
            ui.close();
        }
        if menu_item(ui, Icon::Trash, &format!("Delete {} GIFs", self.picked.len()), Tone::Danger).clicked() {
            actions.push(Action::DeleteGifs(self.picked.clone()));
            ui.close();
        }
    }

    pub(super) fn gif_menu(&mut self, ui: &mut Ui, id: GifId, gi: usize, actions: &mut Vec<Action>) {
        let title = &self.lib.gifs()[gi].title;
        if !title.is_empty() {
            menu_label(ui, title);
        }
        if let Some(why) = self.why_not_loading(id) {
            menu_note(ui, &why, RED);
            if menu_item(ui, Icon::Retry, "Try again", Tone::Plain).clicked() {
                actions.push(Action::Retry(id));
                ui.close();
            }
            menu_sep(ui);
        }
        let other = if self.prefs.auto_send { "Paste without sending" } else { "Paste and send" };
        if menu_item(ui, Icon::Send, other, Tone::Plain).clicked() {
            actions.push(Action::Send { id, invert: true });
            ui.close();
        }
        let copy = if urls::local_name(&self.lib.gifs()[gi].url).is_some() { "Copy file" } else { "Copy link" };
        if menu_item(ui, Icon::Copy, copy, Tone::Plain).clicked() {
            actions.push(Action::CopyLink(id));
            ui.close();
        }
        let select = if self.picked.contains(&id) { "Deselect" } else { "Select" };
        if menu_item(ui, Icon::Check, select, Tone::Plain).clicked() {
            actions.push(Action::Pick { id, range: false });
            ui.close();
        }
        menu_sep(ui);
        self.collections_menu(ui, vec![id], actions);

        menu_sep(ui);
        if menu_item(ui, Icon::Trash, "Delete", Tone::Danger).clicked() {
            actions.push(Action::DeleteGifs(vec![id]));
            ui.close();
        }
    }

    /// The collections part of a GIF menu, for one GIF or several: a check box per collection
    /// (a dash when only some are in it) and a field to make a new one.
    pub(super) fn collections_menu(&mut self, ui: &mut Ui, gifs: Vec<GifId>, actions: &mut Vec<Action>) {
        menu_label(ui, "COLLECTIONS");
        let mut rows = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for c in self.visible_collections() {
                let has = |id: &GifId| self.lib.gif(*id).is_some_and(|g| g.cols.contains(&c.id));
                let n = gifs.iter().filter(|id| has(id)).count();
                let all = n == gifs.len();
                if menu_check(ui, &c.name, all, n > 0) {
                    actions.push(Action::SetMembership { gifs: gifs.clone(), col: c.id, on: !all });
                }
            }
        };
        if self.visible_collections().count() > 6 {
            ScrollArea::vertical().max_height(29.0 * 6.0).auto_shrink([false, true]).show(ui, rows);
        } else {
            rows(ui);
        }
        if menu_field(ui, &mut self.editing.new_name, "New collection", Icon::Plus, "Add") {
            actions.push(Action::NewCollection(std::mem::take(&mut self.editing.new_name), gifs));
        }
    }
}

impl App {
    /// While GIFs are dragged out of the grid: the collections to drop them on, and the GIFs
    /// following the pointer. Dropping anywhere else cancels.
    pub(super) fn filing_panel(&mut self, ui: &Ui, full: Rect, actions: &mut Vec<Action>) {
        let Some(gifs) = &self.grid.filing else { return };
        let (pos, down) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.any_down()));
        let font = semi(12.5);
        let p = ui.ctx().layer_painter(LayerId::new(Order::Foreground, Id::unique("filing")));

        // Chips flow left to right in rows.
        let (left, right) = (full.left() + 22.0, full.right() - 22.0);
        let mut chips = Vec::new();
        let (mut x, mut row) = (left, 0);
        for c in self.visible_collections() {
            let g = p.layout_no_wrap(c.name.clone(), font.clone(), TEXT);
            let w = (g.size().x + 34.0).min(right - left);
            if x + w > right && x > left {
                (x, row) = (left, row + 1);
            }
            chips.push((c.id, x, row, w, g));
            x += w + 6.0;
        }
        let rows = if chips.is_empty() { 1 } else { row + 1 };
        let top = full.bottom() - 30.0 - 34.0 - rows as f32 * 34.0;
        let panel = Rect::from_min_max(pos2(full.left() + 12.0, top), pos2(full.right() - 12.0, full.bottom() - 30.0));
        let shadow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(140) };
        p.add(shadow.as_shape(panel, 12));
        p.rect(panel, 12, RAISED, Stroke::new(1.0, LINE), StrokeKind::Inside);
        let what = if gifs.len() == 1 { "this GIF".to_string() } else { format!("{} GIFs", gifs.len()) };
        let label = if chips.is_empty() {
            "No collections yet. Right-click a GIF to make one.".to_string()
        } else {
            format!("Drop {what} on a collection")
        };
        p.text(pos2(left, top + 18.0), Align2::LEFT_CENTER, label, FontId::proportional(12.0), MUTED);

        let mut target = None;
        for (col, x, row, w, g) in chips {
            let r = Rect::from_min_size(pos2(x, top + 34.0 + row as f32 * 34.0), vec2(w, 28.0));
            let over = pos.is_some_and(|q| r.contains(q));
            if over {
                target = Some(col);
            }
            let has = |id: &GifId| self.lib.gif(*id).is_some_and(|g| g.cols.contains(&col));
            let all = gifs.iter().all(has);
            let hover = fade(ui, Id::unique(("file-chip", col)), over);
            let ring = Stroke::new(1.0, ACCENT.gamma_multiply(0.25 + 0.6 * hover));
            p.rect(r, 8, mix(SURFACE, ACCENT.gamma_multiply(0.35), hover), ring, StrokeKind::Inside);
            let c = r.left_center() + vec2(13.0, 0.0);
            if all {
                paint_icon(&p, Icon::Check, c, 11.0, ACCENT);
            } else {
                paint_icon(&p, Icon::Plus, c, 10.0, mix(MUTED, TEXT, hover));
            }
            let text = Rect::from_min_max(pos2(r.left() + 24.0, r.top()), r.right_bottom() - vec2(8.0, 0.0));
            p.with_clip_rect(text).galley(pos2(text.left(), r.center().y - g.size().y / 2.0), g, TEXT);
        }

        // The dragged GIFs, as a count by the pointer.
        if let Some(q) = pos {
            let tp = ui.ctx().layer_painter(LayerId::new(Order::Tooltip, Id::unique("filing-ghost")));
            let g = tp.layout_no_wrap(gif_count(gifs.len()), font, Color32::WHITE);
            let r = Rect::from_min_size(q + vec2(14.0, 10.0), g.size() + vec2(20.0, 10.0));
            tp.rect_filled(r, r.height() / 2.0, ACCENT);
            tp.galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
        }
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);

        // Released: the drag ends wherever that was.
        if !down && let (Some(gifs), Some(col)) = (self.grid.filing.take(), target) {
            actions.push(Action::SetMembership { gifs, col, on: true });
        }
    }
}

/// A soft light that rolls down the grid over tiles still loading, so a grid that's catching up
/// looks busy rather than broken.
fn loading_glow(ui: &Ui, rect: Rect, strength: f32) {
    let t = ui.input(|i| i.time) as f32;
    let wave = 0.5 + 0.5 * (t * 2.4 - rect.center().y * 0.006).sin();
    let a = (3.0 + 8.0 * wave) * strength;
    ui.painter().rect_filled(rect, RADIUS, Color32::from_white_alpha(a as u8));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}
