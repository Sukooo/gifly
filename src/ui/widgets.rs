//! Small reusable pieces: buttons, cards, rows, keycaps and image painting.

use eframe::egui::{
    self, Align2, Button, Color32, CornerRadius, CursorIcon, FontId, Id, Image, Margin, Mesh, Painter, Pos2, Rect,
    Response, RichText, Sense, Stroke, StrokeKind, TextureHandle, Ui, load::SizedTexture, pos2, vec2,
};

use super::{
    icons::{Icon, paint_icon},
    theme::{ACCENT, GAP, LINE, MUTED, RADIUS, RAISED, SURFACE, TARGET_W, TEXT, fade, mix, semi},
};

/// A square icon button with a hover fill; `on` tints it with the accent.
pub(super) fn icon_button(ui: &Ui, rect: Rect, salt: &str, icon: Icon, tip: &str, on: bool) -> Response {
    let resp = ui.interact(rect, Id::unique(salt), Sense::click());
    let t = fade(ui, resp.id, resp.hovered());
    ui.painter().rect_filled(rect, 10, Color32::from_white_alpha((12.0 * t) as u8));
    let color = if on { ACCENT } else { mix(MUTED, TEXT, t) };
    paint_icon(ui.painter(), icon, rect.center(), 16.0, color);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

/// A filled call-to-action button.
pub(super) fn primary(text: &str, fill: Color32) -> Button<'static> {
    Button::new(RichText::new(text).font(semi(13.5)).color(Color32::WHITE))
        .fill(fill)
        .corner_radius(8)
        .min_size(vec2(0.0, 30.0))
}

/// How many tile columns fit in `width` (at least two), and how wide each one is.
pub(super) fn columns(width: f32) -> (usize, f32) {
    let cols = (((width + GAP) / (TARGET_W + GAP)).floor() as usize).max(2);
    let cell = ((width - GAP * (cols - 1) as f32) / cols as f32).floor();
    (cols, cell)
}

/// Paints `tex` filling `rect`, cropping the overflow like CSS `object-fit: cover`.
pub(super) fn paint_cover(ui: &Ui, rect: Rect, tex: &TextureHandle, tint: Color32) {
    let [w, h] = tex.size();
    let (ta, ca) = (w as f32 / h as f32, rect.width() / rect.height());
    let uv = if ta > ca {
        let f = ca / ta;
        Rect::from_min_max(pos2((1.0 - f) / 2.0, 0.0), pos2((1.0 + f) / 2.0, 1.0))
    } else {
        let f = ta / ca;
        Rect::from_min_max(pos2(0.0, (1.0 - f) / 2.0), pos2(1.0, (1.0 + f) / 2.0))
    };
    Image::from_texture(SizedTexture::from_handle(tex)).uv(uv).tint(tint).corner_radius(RADIUS).paint_at(ui, rect);
}

/// A transparent-to-black gradient over the bottom of a tile, ending in its rounded corners.
pub(super) fn bottom_fade(p: &Painter, tile: Rect, height: f32, alpha: u8) {
    let band = tile.bottom() - f32::from(RADIUS);
    let (clear, dark) = (Color32::TRANSPARENT, Color32::from_black_alpha(alpha));
    let mut m = Mesh::default();
    m.colored_vertex(pos2(tile.left(), tile.bottom() - height), clear);
    m.colored_vertex(pos2(tile.right(), tile.bottom() - height), clear);
    m.colored_vertex(pos2(tile.left(), band), dark);
    m.colored_vertex(pos2(tile.right(), band), dark);
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 2, 3);
    p.add(m);
    let cr = CornerRadius { nw: 0, ne: 0, sw: RADIUS, se: RADIUS };
    p.rect_filled(Rect::from_min_max(pos2(tile.left(), band), tile.right_bottom()), cr, dark);
}

pub(super) fn shadow_text(ui: &Ui, pos: Pos2, text: &str, font: FontId, color: Color32) {
    let p = ui.painter();
    p.text(pos + vec2(0.0, 1.0), Align2::CENTER_CENTER, text, font.clone(), Color32::from_black_alpha(170));
    p.text(pos, Align2::CENTER_CENTER, text, font, color);
}

pub(super) fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).font(semi(11.0)).color(MUTED));
}

pub(super) fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(10)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub(super) fn divider(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, LINE));
}

/// A whole-row clickable setting with a sliding switch on the right.
pub(super) fn toggle_row(ui: &mut Ui, title: &str, sub: &str, on: &mut bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let clicked = resp.clicked();
    if clicked {
        *on = !*on;
    }
    let t = fade(ui, resp.id, *on);
    let p = ui.painter();
    p.text(pos2(r.left(), r.center().y - 8.0), Align2::LEFT_CENTER, title, FontId::proportional(14.0), TEXT);
    p.text(pos2(r.left(), r.center().y + 9.0), Align2::LEFT_CENTER, sub, FontId::proportional(11.5), MUTED);
    let sw = Rect::from_center_size(pos2(r.right() - 17.0, r.center().y), vec2(34.0, 18.0));
    let off = Color32::from_gray(if resp.hovered() { 80 } else { 66 });
    p.rect_filled(sw, 9, mix(off, ACCENT, t));
    let x = egui::lerp(sw.left() + 9.0..=sw.right() - 9.0, t);
    p.circle_filled(pos2(x, sw.center().y), 6.5, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand);
    clicked
}

/// "Ctrl + Shift + G" as keycaps, right-aligned at `right`.
pub(super) fn keycaps(ui: &Ui, right: Pos2, label: &str) {
    let p = ui.painter();
    let mut x = right.x;
    for k in label.rsplit(" + ") {
        let g = p.layout_no_wrap(k.to_string(), semi(12.0), TEXT);
        let size = vec2(g.size().x + 14.0, 22.0);
        let r = Rect::from_min_size(pos2(x - size.x, right.y - size.y / 2.0), size);
        p.rect(r, 6, RAISED, Stroke::new(1.0, LINE), StrokeKind::Inside);
        p.hline(r.x_range().shrink(3.0), r.bottom() - 0.5, Stroke::new(1.0, Color32::from_black_alpha(90)));
        p.galley(r.center() - g.size() / 2.0, g, TEXT);
        x = r.left() - 4.0;
    }
}
