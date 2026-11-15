//! Right-click menus and the rows inside them, including the text-field row Settings reuses.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, Key, Margin, Rect, Response, RichText, Sense, Shape, Stroke,
    StrokeKind, TextEdit, Ui, pos2, text_edit::TextEditState, vec2,
};

use super::{
    icons::{Icon, paint_icon},
    theme::{ACCENT, BG, FAINT, KEY, MUTED, RED, TEXT, Tone, fade, mix, semi},
};

/// Shows `resp`'s right-click menu, which stays open on clicks inside (so collections can be
/// toggled in a row); items close it themselves. Returns whether it's open.
pub(super) fn context_menu(resp: &Response, width: f32, add: impl FnOnce(&mut Ui)) -> bool {
    egui::Popup::context_menu(resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .width(width)
        .show(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            add(ui);
        })
        .is_some()
}

pub(super) fn menu_label(ui: &mut Ui, text: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let g = ui.painter().layout_no_wrap(text.to_string(), semi(10.5), FAINT);
    let pos = pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0);
    ui.painter().with_clip_rect(r.shrink2(vec2(10.0, 0.0))).galley(pos, g, FAINT);
}

/// A few wrapped lines of text, e.g. why a GIF won't load.
pub(super) fn menu_note(ui: &mut Ui, text: &str, color: Color32) {
    let g = ui.painter().layout(text.to_string(), FontId::proportional(12.0), color, ui.available_width() - 20.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y + 8.0), Sense::hover());
    ui.painter().galley(pos2(r.left() + 10.0, r.top() + 4.0), g, color);
}

pub(super) fn menu_sep(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter().hline(r.x_range().shrink(6.0), r.center().y, Stroke::new(1.0, Color32::from_white_alpha(10)));
}

/// A full-width menu row: rounded hover fill, icon, label. Danger rows get a red hover.
pub(super) fn menu_item(ui: &mut Ui, icon: Icon, label: &str, tone: Tone) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let t = fade(ui, resp.id, resp.hovered());
    let p = ui.painter();
    let (fill, fg) = if tone == Tone::Danger {
        (RED.gamma_multiply(0.18), mix(RED, Color32::from_rgb(255, 160, 155), t))
    } else {
        (Color32::from_white_alpha(12), mix(KEY, Color32::WHITE, t))
    };
    p.rect_filled(r, 6, fill.gamma_multiply(t));
    paint_icon(p, icon, pos2(r.left() + 17.0, r.center().y), 14.0, fg);
    p.text(pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.5), fg);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A collection row with a check box; clicking toggles without closing the menu. `mixed` shows a
/// dash: only some of the GIFs it's for are in.
pub(super) fn menu_check(ui: &mut Ui, label: &str, on: bool, mixed: bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    let t = fade(ui, resp.id, resp.hovered());
    let c = fade(ui, resp.id.with("on"), on || mixed);
    let p = ui.painter();
    p.rect_filled(r, 6, Color32::from_white_alpha((12.0 * t) as u8));
    let b = Rect::from_center_size(pos2(r.left() + 17.0, r.center().y), vec2(15.0, 15.0));
    p.rect(
        b,
        4,
        ACCENT.gamma_multiply(c),
        Stroke::new(1.3, mix(Color32::from_gray(95), ACCENT, c)),
        StrokeKind::Inside,
    );
    if mixed && !on {
        let d = vec2(3.5, 0.0);
        p.line_segment([b.center() - d, b.center() + d], Stroke::new(1.8, Color32::WHITE.gamma_multiply(c)));
    } else if c > 0.01 {
        let pts = vec![b.center() + vec2(-3.6, 0.2), b.center() + vec2(-1.0, 2.8), b.center() + vec2(3.8, -2.6)];
        p.add(Shape::line(pts, Stroke::new(1.8, Color32::WHITE.gamma_multiply(c))));
    }
    let fg = if on { TEXT } else { mix(KEY, Color32::WHITE, t) };
    let text_rect = Rect::from_min_max(pos2(r.left() + 34.0, r.top()), pos2(r.right() - 8.0, r.bottom()));
    p.with_clip_rect(text_rect).text(
        text_rect.left_center(),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.5),
        fg,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// A small accent button; returns true when clicked while enabled.
pub(super) fn pill(ui: &Ui, rect: Rect, id: Id, text: &str, enabled: bool) -> bool {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let t = fade(ui, id, enabled && resp.hovered());
    let (fill, color) = if enabled {
        (mix(ACCENT, Color32::WHITE, 0.15 * t), Color32::WHITE)
    } else {
        (Color32::from_white_alpha(10), FAINT)
    };
    ui.painter().rect_filled(rect, 6, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, semi(12.0), color);
    enabled && resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// A full-row text field with an icon and an `action` button; true when submitted by Enter
/// or the button, with non-empty text.
pub(super) fn menu_field(ui: &mut Ui, text: &mut String, hint: &str, icon: Icon, action: &str) -> bool {
    text_row(ui, text, hint, icon, action, false)
}

/// `menu_field` for a secret: masks what's typed and sits on a dark well, so it reads as a
/// field inside a Settings card.
pub(super) fn secret_field(ui: &mut Ui, text: &mut String, hint: &str, icon: Icon, action: &str) -> bool {
    text_row(ui, text, hint, icon, action, true)
}

fn text_row(ui: &mut Ui, text: &mut String, hint: &str, icon: Icon, action: &str, secret: bool) -> bool {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    // Painted under the field, so draw the background first.
    let bg = ui.painter().add(Shape::Noop);
    let resp = ui.put(
        r,
        TextEdit::singleline(text)
            // The margin lives on the frame: a custom frame overrides `.margin()`.
            .frame(egui::Frame::NONE.inner_margin(Margin { left: 34, right: 62, top: 0, bottom: 0 }))
            .min_size(r.size())
            .align(Align2::LEFT_CENTER)
            .font(FontId::proportional(13.5))
            .password(secret)
            .hint_text(RichText::new(hint).color(MUTED)),
    );
    if secret {
        // egui keeps undo history as plain text, so a secret could be brought back with Ctrl+Z.
        if let Some(mut state) = TextEditState::load(ui.ctx(), resp.id) {
            state.clear_undoer();
            state.store(ui.ctx(), resp.id);
        }
    }
    let focus = fade(ui, resp.id, resp.has_focus());
    let hover = fade(ui, resp.id.with("h"), resp.hovered());
    let p = ui.painter();
    let well = if secret { BG } else { Color32::TRANSPARENT };
    let fill = Color32::from_white_alpha((12.0 * hover.max(focus)) as u8);
    p.set(bg, Shape::Vec(vec![Shape::rect_filled(r, 6, well), Shape::rect_filled(r, 6, fill)]));
    paint_icon(p, icon, pos2(r.left() + 17.0, r.center().y), 13.0, mix(KEY, ACCENT, focus));
    let ready = !text.trim().is_empty();
    let btn = Rect::from_min_max(pos2(r.right() - 56.0, r.top() + 4.0), r.right_bottom() - vec2(4.0, 4.0));
    let clicked = pill(ui, btn, resp.id.with("go"), action, ready);
    ready && (clicked || (resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))))
}
