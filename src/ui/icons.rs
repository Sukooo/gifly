//! Vector icons and the app logo, drawn with egui shapes so no font needs the glyphs.

use std::f32::consts::{PI, SQRT_2, TAU};

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, vec2};

use super::theme::ACCENT;

/// A rounded accent square with a white "play" glyph, used for the tray.
pub fn icon_rgba(size: u32) -> (Vec<u8>, u32) {
    let s = size as f32;
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let r = s * 0.22;
            let dx = (fx - fx.clamp(r, s - r)).abs();
            let dy = (fy - fy.clamp(r, s - r)).abs();
            let inside = dx * dx + dy * dy <= r * r;
            // Triangle pointing right, centered.
            let (u, v) = (fx / s - 0.38, (fy / s - 0.5).abs());
            let play = (0.0..0.32).contains(&u) && v < 0.2 * (1.0 - u / 0.32);
            let c = if !inside {
                [0, 0, 0, 0]
            } else if play {
                [255, 255, 255, 255]
            } else {
                [ACCENT.r(), ACCENT.g(), ACCENT.b(), 255]
            };
            px.extend_from_slice(&c);
        }
    }
    (px, size)
}

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Search,
    Back,
    Gear,
    Close,
    Plus,
    Send,
    Copy,
    Trash,
    Pencil,
    Eye,
    EyeOff,
    Key,
    Retry,
    Check,
}

/// Vector icons, so nothing depends on a font having the glyph.
pub(super) fn paint_icon(p: &Painter, icon: Icon, c: Pos2, size: f32, color: Color32) {
    let h = size / 2.0;
    let st = Stroke::new(1.6, color);
    match icon {
        Icon::Search => {
            let o = c - vec2(h * 0.18, h * 0.18);
            let r = h * 0.6;
            p.circle_stroke(o, r, st);
            p.line_segment([o + vec2(r, r) * std::f32::consts::FRAC_1_SQRT_2, c + vec2(h * 0.85, h * 0.85)], st);
        }
        Icon::Retry => {
            // A circle with a gap, and an arrowhead where it ends.
            let r = h * 0.62;
            let at = |deg: f32| c + vec2(deg.to_radians().cos(), deg.to_radians().sin()) * r;
            p.add(Shape::line((0..=16).map(|i| at(-40.0 + 270.0 * i as f32 / 16.0)).collect(), st));
            let (tip, a) = (at(230.0), 230f32.to_radians());
            let (along, out) = (vec2(-a.sin(), a.cos()) * h * 0.38, vec2(a.cos(), a.sin()) * h * 0.38);
            p.line_segment([tip, tip - along + out], st);
            p.line_segment([tip, tip - along - out], st);
        }
        Icon::Back => {
            p.add(Shape::line(
                vec![c + vec2(h * 0.2, -h * 0.6), c + vec2(-h * 0.4, 0.0), c + vec2(h * 0.2, h * 0.6)],
                st,
            ));
        }
        Icon::Close => {
            let a = h * 0.55;
            p.line_segment([c - vec2(a, a), c + vec2(a, a)], st);
            p.line_segment([c + vec2(-a, a), c + vec2(a, -a)], st);
        }
        Icon::Key => {
            let r = h * 0.36;
            let o = c - vec2(h * 0.42, 0.0);
            p.circle_stroke(o, r, st);
            p.line_segment([o + vec2(r, 0.0), c + vec2(h * 0.85, 0.0)], st);
            p.line_segment([c + vec2(h * 0.55, 0.0), c + vec2(h * 0.55, h * 0.4)], st);
            p.line_segment([c + vec2(h * 0.25, 0.0), c + vec2(h * 0.25, h * 0.3)], st);
        }
        Icon::Check => {
            let pts = vec![c + vec2(-h * 0.7, 0.0), c + vec2(-h * 0.2, h * 0.5), c + vec2(h * 0.75, -h * 0.55)];
            p.add(Shape::line(pts, st));
        }
        Icon::Plus => {
            let a = h * 0.7;
            p.line_segment([c - vec2(a, 0.0), c + vec2(a, 0.0)], st);
            p.line_segment([c - vec2(0.0, a), c + vec2(0.0, a)], st);
        }
        Icon::Send => {
            // Paper plane.
            let pts = [
                c + vec2(-h * 0.75, -h * 0.65),
                c + vec2(h * 0.8, 0.0),
                c + vec2(-h * 0.75, h * 0.65),
                c + vec2(-h * 0.4, 0.0),
            ];
            p.add(Shape::closed_line(pts.to_vec(), st));
            p.line_segment([pts[3], c + vec2(h * 0.1, 0.0)], st);
        }
        Icon::Copy => {
            let a = h * 0.5;
            p.rect_stroke(
                Rect::from_center_size(c + vec2(a * 0.35, a * 0.35), vec2(a * 1.5, a * 1.5)),
                2,
                st,
                StrokeKind::Middle,
            );
            p.add(Shape::line(
                vec![c + vec2(-a * 1.1, a * 0.4), c + vec2(-a * 1.1, -a * 1.1), c + vec2(a * 0.4, -a * 1.1)],
                st,
            ));
        }
        Icon::Trash => {
            p.line_segment([c + vec2(-h * 0.75, -h * 0.5), c + vec2(h * 0.75, -h * 0.5)], st);
            p.line_segment([c + vec2(-h * 0.25, -h * 0.8), c + vec2(h * 0.25, -h * 0.8)], st);
            let body = [
                c + vec2(-h * 0.55, -h * 0.5),
                c + vec2(-h * 0.42, h * 0.8),
                c + vec2(h * 0.42, h * 0.8),
                c + vec2(h * 0.55, -h * 0.5),
            ];
            p.add(Shape::line(body.to_vec(), st));
        }
        Icon::Pencil => {
            // A slanted body ending in a point at the bottom left.
            let (tip, end) = (c + vec2(-h * 0.7, h * 0.7), c + vec2(h * 0.6, -h * 0.6));
            let (d, n) = (vec2(h, -h) * 0.25, vec2(h, h) * 0.17);
            p.add(Shape::closed_line(vec![tip, tip + d + n, end + n, end - n, tip + d - n], st));
        }
        Icon::Eye => {
            let u = h / 11.0;
            p.add(Shape::closed_line(eye(c, u), st));
            p.add(Shape::closed_line(ring(c, 3.0 * u, 32), st));
        }
        Icon::EyeOff => {
            // The eye, struck through: the outline breaks around the slash and half the pupil is left.
            let u = h / 11.0;
            let side = |q: Pos2| (q.y - c.y - (q.x - c.x)) / SQRT_2; // signed distance from the slash
            let mut pts = eye(c, u);
            // Start inside a gap, so no stretch of outline wraps around the end of the list.
            let gap = pts.iter().position(|&q| side(q).abs() < 2.6 * u).unwrap_or(0);
            pts.rotate_left(gap);
            for run in pts.split(|&q| side(q).abs() < 2.6 * u).filter(|r| r.len() > 1) {
                p.add(Shape::line(run.to_vec(), st));
            }
            p.add(Shape::line(ring(c, 3.0 * u, 32).into_iter().filter(|&q| side(q) > 0.0).collect(), st));
            let end = Vec2::splat(9.0 * u);
            p.line_segment([c - end, c + end], st);
            p.circle_filled(c - end, st.width / 2.0, color);
            p.circle_filled(c + end, st.width / 2.0, color);
        }
        Icon::Gear => {
            // Six softly squared teeth around a hub.
            let u = h / 11.0;
            let pts = (0..128)
                .map(|k| {
                    let a = k as f32 / 128.0 * TAU;
                    let tooth = (2.2 * (6.0 * a).cos()).tanh() / 2.2f32.tanh();
                    c + vec2(a.cos(), a.sin()) * (8.2 + 1.9 * tooth) * u
                })
                .collect();
            p.add(Shape::closed_line(pts, st));
            p.add(Shape::closed_line(ring(c, 3.2 * u, 32), st));
        }
    }
}

/// An eye outline 20 units wide whose ends round off instead of meeting in a point.
fn eye(c: Pos2, u: f32) -> Vec<Pos2> {
    let lid = |s: f32| {
        (0..24).map(move |k| {
            let x = -s * (PI * k as f32 / 24.0).cos();
            c + vec2(x * 10.0, -s * 6.6 * (1.0 - x * x).powf(0.75)) * u
        })
    };
    lid(1.0).chain(lid(-1.0)).collect()
}

/// `n` points around a circle, for strokes that need the points rather than a shape.
fn ring(c: Pos2, r: f32, n: usize) -> Vec<Pos2> {
    (0..n).map(|k| c + Vec2::angled(k as f32 / n as f32 * TAU) * r).collect()
}

/// The app icon: an accent rounded square with a white play triangle (as in assets/gifly.ico).
pub(super) fn paint_logo(p: &Painter, r: Rect) {
    let s = r.width();
    p.rect_filled(r, s * 0.22, ACCENT);
    let at = |x: f32, y: f32| r.min + vec2(x, y) * s;
    p.add(Shape::convex_polygon(vec![at(0.38, 0.3), at(0.7, 0.5), at(0.38, 0.7)], Color32::WHITE, Stroke::NONE));
}
