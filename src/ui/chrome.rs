//! The frame around every view: drag handle, search bar, footer, resize grip and toasts.

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, FontId, Id, Layout, Rect, RectAlign, Response, RichText, Sense, Stroke,
    StrokeKind, TextEdit, Ui, UiBuilder, Vec2, ViewportCommand, pos2, vec2,
};

use crate::app::{Action, App, View, gif_count};

use super::{
    icons::{Icon, paint_icon},
    search_id,
    theme::{ACCENT, BG, BLURPLE, FAINT, KEY, LINE, MUTED, RAISED, RED, SURFACE, TEXT, Tone, fade, mix, semi},
    widgets::icon_button,
};

impl App {
    pub(super) fn header(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // Drag handle.
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::drag());
        let t = fade(ui, resp.id, resp.hovered() || resp.dragged());
        let pill = Rect::from_center_size(r.center(), vec2(32.0 + 8.0 * t, 4.0));
        ui.painter().rect_filled(pill, 2, mix(Color32::from_gray(58), Color32::from_gray(120), t));
        if resp.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }

        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
        let in_settings = self.view == View::Settings;
        // Square buttons the height of the search bar, so hover fills line up with it.
        let gear = Rect::from_min_size(pos2(row.right() - row.height(), row.top()), Vec2::splat(row.height()));
        if icon_button(ui, gear, "gear", Icon::Gear, "Settings", in_settings).clicked() {
            actions.push(Action::Open(if in_settings { View::Home } else { View::Settings }));
        }
        if in_settings {
            let back = Rect::from_min_size(pos2(row.left(), row.top() + 3.0), vec2(32.0, 32.0));
            if icon_button(ui, back, "back", Icon::Back, "Back (Esc)", false).clicked() {
                actions.push(Action::Open(View::Home));
            }
            let pos = pos2(back.right() + 6.0, row.center().y);
            ui.painter().text(pos, Align2::LEFT_CENTER, "Settings", semi(16.0), TEXT);
        } else {
            let mut right = gear.left() - 4.0;
            if self.lib.collections().iter().any(|c| c.hidden) {
                let eye = gear.translate(vec2(-row.height() - 2.0, 0.0));
                let (icon, tip) =
                    if self.reveal { (Icon::Eye, "Re-hide (Ctrl+H)") } else { (Icon::EyeOff, "Show hidden (Ctrl+H)") };
                if icon_button(ui, eye, "reveal", icon, tip, self.reveal).clicked() {
                    actions.push(Action::Reveal);
                }
                right = eye.left() - 4.0;
            }
            self.search_bar(ui, Rect::from_min_max(row.min, pos2(right, row.bottom())), actions);
        }
    }

    pub(super) fn search_bar(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let focused = ui.memory(|m| m.has_focus(search_id()));
        let t = fade(ui, search_id().with("ring"), focused);
        // The field almost always has focus, so its ring stays soft and brightens on hover.
        let hv = fade(ui, search_id().with("hover"), ui.rect_contains_pointer(rect));
        let ring = Stroke::new(1.0, mix(LINE, ACCENT.gamma_multiply(0.3 + 0.3 * hv), t));
        ui.painter().rect(rect, 10, SURFACE, ring, StrokeKind::Inside);
        // Clicking the padding focuses the field too.
        if ui.interact(rect, search_id().with("bar"), Sense::click()).on_hover_cursor(CursorIcon::Text).clicked() {
            self.win.focus_search = true;
        }

        let inner = rect.shrink2(vec2(11.0, 0.0));
        let mut c = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        c.spacing_mut().item_spacing.x = 7.0;

        let (ir, _) = c.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        paint_icon(c.painter(), Icon::Search, ir.center(), 15.0, mix(MUTED, TEXT, t));

        // Current view as a chip; clicking it goes back to everything.
        if self.view != View::Home {
            let accent = if self.view == View::Imports { BLURPLE } else { ACCENT };
            let g = c.painter().layout_no_wrap(self.view_name(self.view), semi(12.5), TEXT);
            let (r, resp) = c.allocate_exact_size(vec2(g.size().x + 28.0, 24.0), Sense::click());
            let h = fade(&c, resp.id, resp.hovered());
            let p = c.painter();
            p.rect_filled(r, 7, accent.gamma_multiply(0.22 + 0.14 * h));
            let fg = mix(accent, Color32::WHITE, 0.55 + 0.3 * h);
            paint_icon(p, Icon::Back, r.left_center() + vec2(11.0, 0.0), 10.0, fg);
            p.galley_with_override_text_color(pos2(r.left() + 19.0, r.center().y - g.size().y / 2.0), g, fg);
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Back to all (Backspace)").clicked() {
                actions.push(Action::Open(View::Home));
            }
        }

        let clear_w = if self.query.is_empty() { 0.0 } else { 27.0 };
        let hint = if self.view == View::Home { "Search your GIFs" } else { "Search" };
        let r = c.add(
            TextEdit::singleline(&mut self.query)
                .id(search_id())
                .hint_text(RichText::new(hint).color(FAINT))
                .frame(egui::Frame::NONE)
                .desired_width((c.available_width() - clear_w).max(24.0))
                .font(FontId::proportional(15.0)),
        );
        if self.win.focus_search {
            r.request_focus();
            self.win.focus_search = false;
        }
        if r.changed() {
            self.dirty = true;
            self.grid.selected = None;
            self.grid.scroll_to = Some(0.0);
        }

        if !self.query.is_empty() {
            let (cr, resp) = c.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
            let h = fade(&c, resp.id, resp.hovered());
            c.painter().circle_filled(cr.center(), 10.0, Color32::from_white_alpha((8.0 + 14.0 * h) as u8));
            paint_icon(c.painter(), Icon::Close, cr.center(), 11.0, mix(MUTED, TEXT, h));
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.clear_search();
            }
        }
    }

    /// The bottom line: how many GIFs are showing, or while some are selected, what to do with them.
    pub(super) fn footer(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
        if !self.showing_grid() {
            return;
        }
        if self.picked.is_empty() {
            // Clear of the resize grip in the corner.
            let pos = pos2(r.right() - 10.0, r.center().y);
            ui.painter().text(pos, Align2::RIGHT_CENTER, gif_count(self.list.len()), FontId::proportional(11.0), FAINT);
            return;
        }

        let g = ui.painter().layout_no_wrap(format!("{} selected", self.picked.len()), semi(12.0), TEXT);
        ui.painter().galley(pos2(r.left() + 2.0, r.center().y - g.size().y / 2.0), g.clone(), TEXT);
        let mut x = r.left() + g.size().x + 10.0;

        let cols = bar_button(ui, &mut x, r, "Collections", Tone::Plain);
        egui::Popup::menu(&cols)
            .align(RectAlign::TOP_START)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .width(220.0)
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                self.collections_menu(ui, self.picked.clone(), actions);
            });
        if bar_button(ui, &mut x, r, "Delete", Tone::Danger).clicked() {
            actions.push(Action::DeleteGifs(self.picked.clone()));
        }
        if bar_button(ui, &mut x, r, "Clear", Tone::Plain).on_hover_text("Esc").clicked() {
            actions.push(Action::ClearPicks);
        }
    }

    /// A faint highlight along the window's top edge, so it stands off dark wallpapers.
    pub(super) fn top_edge(ui: &Ui, full: Rect) {
        ui.painter().hline(full.x_range(), full.top() + 0.5, Stroke::new(1.0, Color32::from_white_alpha(14)));
    }

    pub(super) fn resize_grip(&self, ui: &Ui, full: Rect) {
        let r = Rect::from_min_size(full.right_bottom() - vec2(14.0, 14.0), vec2(14.0, 14.0));
        let resp = ui.interact(r, Id::unique("resize-grip"), Sense::drag()).on_hover_cursor(CursorIcon::ResizeNwSe);
        if resp.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
        }
        let c = Color32::from_gray(if resp.hovered() { 140 } else { 70 });
        for (dx, dy) in [(4.0, 4.0), (8.0, 4.0), (4.0, 8.0)] {
            ui.painter().circle_filled(r.right_bottom() - vec2(dx, dy), 1.1, c);
        }
    }

    /// The last change's message, bottom right, with an Undo button when it can be undone. It
    /// stays while the pointer is on it.
    pub(super) fn paint_toast(&mut self, ui: &Ui, full: Rect, actions: &mut Vec<Action>) {
        let now = ui.input(|i| i.time);
        let Some(t) = &mut self.toast else { return };
        let left = t.until - now;
        if left <= 0.0 {
            self.toast = None;
            return;
        }
        // Fade out over the last quarter second.
        let a = (left / 0.25).min(1.0) as f32;
        let p = ui.painter();
        let ug = (!t.undo.is_empty()).then(|| p.layout_no_wrap("Undo".into(), semi(13.0), ACCENT));
        let uw = ug.as_ref().map_or(0.0, |u| u.size().x + 22.0);
        let wrap = (full.width() - 24.0 - 36.0 - uw).max(80.0);
        let g = p.layout(t.msg.clone(), FontId::proportional(13.0), TEXT.gamma_multiply(a), wrap);
        let size = vec2(g.size().x + 36.0 + uw, g.size().y + 16.0);
        // Above the footer, clear of the resize grip.
        let r = Rect::from_min_size(full.right_bottom() - vec2(12.0, 30.0) - size, size);
        if ui.rect_contains_pointer(r) {
            t.until = t.until.max(now + 1.5);
        }
        let shadow =
            egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha((110.0 * a) as u8) };
        p.add(shadow.as_shape(r, 10));
        p.rect(r, 10, RAISED.gamma_multiply(a), Stroke::new(1.0, LINE.gamma_multiply(a)), StrokeKind::Inside);
        let dot = pos2(r.left() + 14.0, r.top() + 8.0 + g.rows[0].height() / 2.0);
        p.circle_filled(dot, 3.0, ACCENT.gamma_multiply(a));
        p.galley(pos2(r.left() + 24.0, r.top() + 8.0), g, TEXT);
        if let Some(ug) = ug {
            let br = Rect::from_min_max(pos2(r.right() - uw, r.top()), r.max);
            let line = Stroke::new(1.0, Color32::from_white_alpha((16.0 * a) as u8));
            p.vline(br.left(), br.y_range().shrink(7.0), line);
            let resp = ui.interact(br, Id::unique("toast-undo"), Sense::click());
            let h = fade(ui, resp.id, resp.hovered());
            p.rect_filled(br.shrink(4.0), 7, ACCENT.gamma_multiply(0.18 * h * a));
            let fg = mix(ACCENT, Color32::WHITE, 0.25 + 0.35 * h).gamma_multiply(a);
            p.galley_with_override_text_color(br.center() - ug.size() / 2.0, ug, fg);
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Ctrl+Z").clicked() {
                actions.push(Action::Undo);
            }
        }
        if left < 0.25 {
            ui.ctx().request_repaint();
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(left - 0.25));
        }
    }

    /// Files dragged over the picker: says where they'll go.
    pub(super) fn drop_overlay(&self, ui: &Ui, full: Rect) {
        let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique("drop")));
        let r = full.shrink(8.0);
        p.rect(r, 14, BG.gamma_multiply(0.88), Stroke::new(1.5, ACCENT.gamma_multiply(0.7)), StrokeKind::Inside);
        let to = match self.view {
            View::Collection(_) => format!("Drop to add to {}", self.view_name(self.view)),
            _ => "Drop to add to your GIFs".into(),
        };
        p.text(r.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, to, semi(15.0), TEXT);
        let kinds = "GIF, PNG, JPEG or WebP";
        p.text(r.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER, kinds, FontId::proportional(12.0), MUTED);
    }
}

/// A text button in the selection bar, placed at `x` and moving it along. Danger buttons turn red
/// on hover.
fn bar_button(ui: &Ui, x: &mut f32, row: Rect, label: &str, tone: Tone) -> Response {
    let g = ui.painter().layout_no_wrap(label.into(), FontId::proportional(12.0), KEY);
    let r = Rect::from_min_size(pos2(*x, row.top()), vec2(g.size().x + 16.0, row.height()));
    *x = r.right() + 2.0;
    let resp = ui.interact(r, Id::unique(("bar", label)), Sense::click());
    let t = fade(ui, resp.id, resp.hovered());
    let (fill, hot) = if tone == Tone::Danger {
        (RED.gamma_multiply(0.18), Color32::from_rgb(255, 160, 155))
    } else {
        (Color32::from_white_alpha(12), Color32::WHITE)
    };
    ui.painter().rect_filled(r, 6, fill.gamma_multiply(t));
    ui.painter().galley_with_override_text_color(r.center() - g.size() / 2.0, g, mix(KEY, hot, t));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
