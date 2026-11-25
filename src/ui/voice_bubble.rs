//! Voice message bubble (P3-R4, fixes AM §12.A): ▶/⏸ in the 40 px icon box, a seek bar and the time; no file name.
use crate::{
    i18n::{tr, Key, Lang},
    media::player::PlayState,
    ui::{message_menu::Action, theme::Palette},
};
use eframe::egui::{pos2, vec2, Color32, CursorIcon, FontId, Galley, Id, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo};
use std::sync::Arc;

const ICON: f32 = 40.;
const BAR_H: f32 = 4.;
/// Same height as the file panel (40 px icon box + 8).
pub const SIZE: Vec2 = vec2(220., ICON + 8.);

pub struct Voice {
    playing: bool,
    /// Loaded in the player: only then does the bar seek.
    loaded: bool,
    /// Played fraction, 0 unless this message is loaded in the player.
    frac: f32,
    time: Arc<Galley>,
}

/// «0:07», «1:05».
pub fn clock(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `len_ms`: the file's length (None: not probed yet, or unreadable: «--:--»); `play`: the player's state.
pub fn prepare(ui: &Ui, guid: i64, len_ms: Option<u64>, play: PlayState, p: &Palette) -> Voice {
    let loaded = play.guid == Some(guid);
    let len = if loaded && play.len_ms > 0 { Some(play.len_ms) } else { len_ms };
    let total = len.map_or("--:--".to_string(), clock);
    let text = if loaded { format!("{} / {total}", clock(play.pos_ms)) } else { total };
    let frac = match len {
        Some(len) if loaded && len > 0 => (play.pos_ms as f32 / len as f32).min(1.),
        _ => 0.,
    };
    // Styled like the file panel's size line.
    let time = ui.painter().layout_no_wrap(text, FontId::proportional(13.), p.text.gamma_multiply(0.7));
    Voice { playing: play.is_playing(guid), loaded, frac, time }
}

impl Voice {
    /// The icon box and the rest of the panel toggle playback; the bar seeks (registered after the panel, so it wins).
    pub fn show(self, ui: &Ui, min: Pos2, id: Id, p: &Palette, lang: Lang) -> (Response, Option<Action>) {
        let rect = Rect::from_min_size(min, SIZE);
        let icon = Rect::from_min_size(pos2(min.x, rect.center().y - ICON / 2.), Vec2::splat(ICON));
        let label = tr(lang, if self.playing { Key::Pause } else { Key::Play });
        let resp = ui.interact(rect, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, tr(lang, Key::VoiceMessage)));
        let button = ui.interact(icon, id.with("play"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        button.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
        let x0 = icon.right() + 10.;
        let bar = Rect::from_min_max(pos2(x0, rect.center().y - 9.), pos2(rect.right() - 6., rect.center().y - 9. + BAR_H));
        // Not loaded: hover only, so a click on the bar reaches the panel and plays from the start.
        let sense = if self.loaded { Sense::click_and_drag() } else { Sense::hover() };
        let mut seek = ui.interact(bar.expand2(vec2(6., 7.)), id.with("seek"), sense);
        if self.loaded {
            seek = seek.on_hover_cursor(CursorIcon::PointingHand);
        }
        seek.widget_info(|| WidgetInfo { value: Some(f64::from(self.frac)), ..WidgetInfo::new(Role::ProgressIndicator) });

        let painter = ui.painter();
        painter.rect_filled(icon, 6, p.file_icon);
        play_pause(painter, icon.center(), self.playing, p.text);
        painter.rect_filled(bar, 2, p.text.gamma_multiply(0.2));
        let done = pos2(bar.left() + bar.width() * self.frac, bar.center().y);
        painter.rect_filled(Rect::from_min_max(bar.min, pos2(done.x, bar.max.y)), 2, p.accent);
        painter.circle_filled(done, 5., p.accent);
        painter.galley(pos2(x0, bar.bottom() + 7.), self.time, p.text);

        let to = seek.interact_pointer_pos().filter(|_| seek.clicked() || seek.dragged());
        let action = match to {
            Some(at) => Some(Action::Seek(((at.x - bar.left()) / bar.width()).clamp(0., 1.))),
            None => (resp.clicked() || button.clicked()).then_some(Action::Play),
        };
        (resp.union(button).union(seek), action)
    }
}

/// ▶ (ic_play_outline) or ⏸ (ic_pause_outline), 24 px class.
pub(crate) fn play_pause(painter: &Painter, c: Pos2, playing: bool, color: Color32) {
    if playing {
        for dx in [-4., 4.] {
            painter.line_segment([c + vec2(dx, -7.), c + vec2(dx, 7.)], Stroke::new(3., color));
        }
    } else {
        let tri = vec![c + vec2(-5., -8.), c + vec2(-5., 8.), c + vec2(8., 0.)];
        painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
    }
}

#[cfg(test)]
mod tests {
    use super::clock;
    #[test]
    fn clock_rounds_to_seconds() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(1990), "0:02");
        assert_eq!(clock(65_000), "1:05");
        assert_eq!(clock(600_000), "10:00");
    }
}
