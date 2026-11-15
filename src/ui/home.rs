//! Home: a tile per collection (and Recent, Copied, Unsorted, Discord imports), reorderable by dragging.

use eframe::egui::{
    self, Align2, Button, Color32, CursorIcon, FontId, Id, Key, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind,
    TextEdit, Ui, Vec2, vec2,
};

use crate::app::{Action, App, View};

use super::{
    icons::{Icon, paint_icon},
    menu::{context_menu, menu_field, menu_item, menu_label, menu_sep, pill},
    theme::{ACCENT, BLURPLE, FAINT, GAP, LINE, MUTED, RADIUS, SURFACE, TEXT, Tone, fade, mix, semi},
    widgets::{columns, paint_cover, primary, shadow_text},
};

impl App {
    pub(super) fn home(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if self.lib.gifs().is_empty() && self.lib.collections().is_empty() && !self.editing.creating {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.24);
                let (r, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 24.0, SURFACE);
                paint_icon(ui.painter(), Icon::Plus, r.center(), 20.0, ACCENT);
                ui.add_space(6.0);
                ui.label(RichText::new("No GIFs yet").font(semi(17.0)).color(TEXT));
                ui.label(RichText::new("Copy any GIF link and it lands here.").color(MUTED));
                ui.add_space(10.0);
                if ui.add(primary("Import Discord favorites", BLURPLE)).clicked() {
                    actions.push(Action::Open(View::Settings));
                }
                if ui.add(Button::new("New collection").min_size(vec2(0.0, 30.0))).clicked() {
                    self.editing.creating = true;
                    self.editing.focus_new = true;
                }
            });
            return;
        }

        let (cols, cw) = columns(ui.available_width());
        self.grid.tile_px = (cw * ui.ctx().pixels_per_point()).round() as u32;
        let size = vec2(cw, (cw * 0.62).round());
        let n = self.home.len() + 1;
        ScrollArea::vertical().auto_shrink(false).id_salt("home").show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            for row in 0..n.div_ceil(cols) {
                ui.horizontal(|ui| {
                    for i in row * cols..(row * cols + cols).min(n) {
                        if i < self.home.len() {
                            self.home_tile(ui, i, cols, size, actions);
                        } else {
                            self.new_tile(ui, size, actions);
                        }
                    }
                });
            }
        });
        if self.editing.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
            self.editing.drag = None;
            actions.push(Action::SaveOrder);
        }
    }

    pub(super) fn home_tile(&mut self, ui: &mut Ui, i: usize, cols: usize, size: Vec2, actions: &mut Vec<Action>) {
        let (view, hidden) = (self.home[i].view, self.home[i].hidden);
        let label = self.home[i].label.clone();
        let (slot, _) = ui.allocate_exact_size(size, Sense::hover());
        // A stable id, so a dragged tile keeps its drag as tiles reorder under it.
        let resp = ui.interact(slot, Id::unique(("home", view)), Sense::click_and_drag());
        let dragging = self.editing.drag == Some(view);

        // Tiles glide to their new place when the order changes. Animated in grid units,
        // so scrolling and resizing don't lag behind.
        let (c, r) = ((i % cols) as f32, (i / cols) as f32);
        let step = size + Vec2::splat(GAP);
        let origin = slot.min - vec2(c * step.x, r * step.y);
        let ac = ui.ctx().animate_value_with_time(resp.id.with("col"), c, 0.16);
        let ar = ui.ctx().animate_value_with_time(resp.id.with("row"), r, 0.16);
        let rect = Rect::from_min_size(origin + vec2(ac * step.x, ar * step.y), size);
        // Scrolled out of sight: nothing to draw, and no cover worth loading.
        if !dragging && !ui.is_rect_visible(slot.union(rect)) {
            return;
        }

        if dragging {
            // Where it lands on release...
            let fill = ACCENT.gamma_multiply(0.08);
            ui.painter().rect(rect, RADIUS, fill, Stroke::new(1.5, ACCENT.gamma_multiply(0.6)), StrokeKind::Inside);
            // ...and the tile itself, lifted, following the pointer from where it was grabbed.
            let grab = ui.data(|d| d.get_temp::<Vec2>(resp.id.with("grab"))).unwrap_or_default();
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                let ghost = Rect::from_min_size(p + grab, size).expand(4.0);
                let ctx = ui.ctx().clone();
                egui::Area::new(resp.id.with("ghost"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(ghost.min)
                    .interactable(false)
                    .show(&ctx, |ui| {
                        ui.allocate_exact_size(ghost.size(), Sense::hover());
                        let shadow = egui::Shadow {
                            offset: [0, 10],
                            blur: 26,
                            spread: 0,
                            color: Color32::from_black_alpha(150),
                        };
                        ui.painter().add(shadow.as_shape(ghost, RADIUS));
                        self.paint_tile(ui, ghost, i, 1.0);
                    });
            }
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else {
            let h = fade(ui, resp.id, resp.hovered() && self.editing.drag.is_none());
            self.paint_tile(ui, rect, i, h);
        }

        if resp.clicked() {
            actions.push(Action::Open(view));
        }
        if resp.drag_started() {
            self.editing.drag = Some(view);
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                ui.data_mut(|d| d.insert_temp(resp.id.with("grab"), slot.min - p));
            }
        }
        // Live reorder: the dragged tile takes the place of whichever tile is under the pointer.
        if let Some(d) = self.editing.drag
            && d != view
            && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| slot.contains(p))
        {
            actions.push(Action::MoveTile(d, view));
        }
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        let View::Collection(cid) = view else { return };
        let open = context_menu(&resp, 210.0, |ui| {
            if self.editing.renaming.as_ref().map(|r| r.0) != Some(cid) {
                self.editing.renaming = Some((cid, label.clone()));
            }
            let Some((_, name)) = self.editing.renaming.as_mut() else { return };
            menu_label(ui, "RENAME");
            if menu_field(ui, name, "Collection name", Icon::Pencil, "Save") {
                actions.push(Action::RenameCollection(cid, name.clone()));
                ui.close();
            }
            menu_sep(ui);
            let (icon, text) =
                if hidden { (Icon::Eye, "Unhide collection") } else { (Icon::EyeOff, "Hide collection") };
            if menu_item(ui, icon, text, Tone::Plain).clicked() {
                actions.push(Action::HideCollection { id: cid, hidden: !hidden });
                ui.close();
            }
            if menu_item(ui, Icon::Trash, "Delete collection", Tone::Danger).clicked() {
                actions.push(Action::DeleteCollection(cid));
                ui.close();
            }
        });
        // A name typed and left unsaved isn't shown the next time the menu opens.
        if !open && self.editing.renaming.as_ref().is_some_and(|r| r.0 == cid) {
            self.editing.renaming = None;
        }
    }

    /// A Home tile's look: cover, outline, name and count. `h` is the hover highlight (0..1).
    pub(super) fn paint_tile(&mut self, ui: &Ui, rect: Rect, i: usize, h: f32) {
        let (view, cover, count, hidden) =
            (self.home[i].view, self.home[i].cover, self.home[i].count, self.home[i].hidden);
        let accent = if view == View::Imports { BLURPLE } else { ACCENT };
        ui.painter().rect_filled(rect, RADIUS, SURFACE);
        // Covers play like the grid does; hidden ones stay still, being dimmed anyway.
        let still = hidden || (self.prefs.hover_play && h == 0.0);
        let tex = cover.and_then(|id| self.lib.index(id)).and_then(|gi| {
            let thumb = self.thumb(gi, false)?;
            Some(if still { thumb } else { self.anim_frame(gi, false).unwrap_or(thumb) })
        });
        let base = if hidden { 50.0 } else { 92.0 };
        match tex {
            Some(t) => paint_cover(ui, rect, &t, Color32::from_gray((base + 70.0 * h) as u8)),
            None => {
                ui.painter().rect_filled(rect, RADIUS, Color32::from_white_alpha((7.0 * h) as u8));
            }
        }
        ui.painter().rect_stroke(rect, RADIUS, Stroke::new(1.5, mix(LINE, accent, h)), StrokeKind::Inside);
        if hidden {
            let c = rect.left_top() + vec2(16.0, 16.0);
            ui.painter().circle_filled(c, 11.0, Color32::from_black_alpha(140));
            paint_icon(ui.painter(), Icon::EyeOff, c, 13.0, Color32::from_gray(200));
        }
        let label = &self.home[i].label;
        shadow_text(ui, rect.center() - vec2(0.0, 8.0), label, semi(15.0), Color32::WHITE);
        shadow_text(
            ui,
            rect.center() + vec2(0.0, 11.0),
            &count.to_string(),
            FontId::proportional(11.5),
            Color32::from_gray(200),
        );
    }

    pub(super) fn new_tile(&mut self, ui: &mut Ui, size: Vec2, actions: &mut Vec<Action>) {
        if self.editing.creating {
            // The tile itself is the input: one accent outline, no inner field frame.
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect(rect, RADIUS, SURFACE, Stroke::new(1.5, ACCENT), StrokeKind::Inside);
            let field = Rect::from_center_size(rect.center() - vec2(0.0, 7.0), vec2(rect.width() - 20.0, 26.0));
            let r = ui.put(
                field,
                TextEdit::singleline(&mut self.editing.new_name)
                    .frame(egui::Frame::NONE)
                    .align(Align2::CENTER_CENTER)
                    .font(semi(15.0))
                    .hint_text(RichText::new("Collection name").color(FAINT)),
            );
            let ready = !self.editing.new_name.trim().is_empty();
            let btn = Rect::from_center_size(rect.center() + vec2(0.0, 22.0), vec2(72.0, 24.0));
            let clicked = pill(ui, btn, r.id.with("create"), "Create", ready);
            if self.editing.focus_new {
                r.request_focus();
                self.editing.focus_new = false;
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ready && (clicked || enter) {
                actions.push(Action::NewCollection(self.editing.new_name.clone(), Vec::new()));
            } else if r.lost_focus() {
                // Clicks inside the tile (e.g. on Create) keep it open; anything else cancels.
                if ui.input(|i| i.pointer.any_down() && i.pointer.interact_pos().is_some_and(|p| rect.contains(p))) {
                    r.request_focus();
                } else {
                    self.editing.creating = false;
                }
            }
            return;
        }
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let h = fade(ui, resp.id, resp.hovered());
        let c = mix(Color32::from_gray(90), ACCENT, h);
        let p = ui.painter();
        p.rect(
            rect,
            RADIUS,
            Color32::from_white_alpha((6.0 * h) as u8),
            Stroke::new(1.0, mix(LINE, ACCENT, h)),
            StrokeKind::Inside,
        );
        paint_icon(p, Icon::Plus, rect.center() - vec2(0.0, 9.0), 14.0, c);
        p.text(rect.center() + vec2(0.0, 11.0), Align2::CENTER_CENTER, "New collection", FontId::proportional(12.5), c);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            self.editing.creating = true;
            self.editing.focus_new = true;
        }
    }
}
