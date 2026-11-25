//! Message bubble geometry (Android 3.3.7): radius 4, sender-side bottom corner square, 8x8 tail outside the body.
use eframe::egui::{pos2, Pos2, Rect, vec2};

pub const RADIUS: f32 = 4.0;
pub const TAIL: f32 = 8.0;

pub struct BubbleGeom {
    pub body: Rect,
    pub tail: [Pos2; 3],
}

pub fn max_width(view_width: f32) -> f32 {
    (view_width * 0.7).min(520.0)
}

/// Body = content padded 6 top/bottom and 16 on the tail side / 8 on the other (message_background*.xml);
/// the tail hangs outside the sender-side bottom corner.
pub fn geom(content: Rect, outgoing: bool) -> BubbleGeom {
    let (l, r) = if outgoing { (8.0, 16.0) } else { (16.0, 8.0) };
    let body = Rect::from_min_max(content.min - vec2(l, 6.0), content.max + vec2(r, 6.0));
    let (x, dx) = if outgoing { (body.right(), TAIL) } else { (body.left(), -TAIL) };
    let b = body.bottom();
    BubbleGeom { body, tail: [pos2(x, b - TAIL), pos2(x, b), pos2(x + dx, b)] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Color32, FontId};
    use egui_kittest::Harness;

    #[test] fn tail_on_sender_side() {
        let g = geom(Rect::from_min_size(pos2(100., 0.), vec2(200., 40.)), true);
        assert!(g.tail.iter().all(|p| p.x >= g.body.right() - 0.5));
        assert_eq!(g.tail.iter().map(|p| p.y).fold(0., f32::max), g.body.bottom());
        let g = geom(Rect::from_min_size(pos2(100., 0.), vec2(200., 40.)), false);
        assert!(g.tail.iter().all(|p| p.x <= g.body.left() + 0.5));
    }
    #[test] fn padding_mirrors_direction() {
        let c = Rect::from_min_size(pos2(100., 50.), vec2(200., 40.));
        let (i, o) = (geom(c, false).body, geom(c, true).body);
        assert_eq!((c.left() - i.left(), i.right() - c.right()), (16., 8.));
        assert_eq!((c.left() - o.left(), o.right() - c.right()), (8., 16.));
        assert_eq!((c.top() - i.top(), i.bottom() - c.bottom()), (6., 6.));
    }
    #[test] fn bubble_wraps_unbroken_text() {
        let mut h = Harness::new_ui(|ui| {
            let wrap = max_width(800.) - 24.;
            let galley = ui.painter().layout("a".repeat(2000), FontId::proportional(14.), Color32::WHITE, wrap);
            assert!(galley.rect.width() <= 560., "width {}", galley.rect.width());
            assert!(galley.rows.len() > 1);
            ui.label("ok");
        });
        h.run();
    }
}
