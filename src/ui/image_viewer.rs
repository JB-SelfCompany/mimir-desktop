//! Full-window image viewer (Android PictureActivity): the picture fitted to the window and «Сохранить»; Esc or a
//! click outside the picture closes it.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key},
    media::{self, thumbs::Thumb},
    ui::{attachment_bubble::fit, theme::{palette, primary_button}},
};
use eframe::egui::{vec2, Align, Color32, Context, Frame, Id, Image, Layout, Modal, RichText, Role, Sense, WidgetInfo};

/// Long edge of the viewer's copy of a picture.
// ponytail: pictures larger than 2048 px are shown scaled down to it; no zoom or pan.
pub const VIEW_PX: u32 = 2048;
/// Free space around the picture.
const MARGIN: f32 = 48.;

pub fn show(app: &mut MimirDesktop, ctx: &Context) {
    let Some((path, name)) = app.state.viewer.clone() else { return };
    let (lang, p) = (app.lang, palette(ctx.global_style().visuals.dark_mode));
    let thumb = app.state.thumbs.get_sized(ctx, &path, VIEW_PX);
    // Room for the picture below the button row.
    let room = (ctx.content_rect().size() - vec2(2. * MARGIN, 2. * MARGIN + 40.)).max(vec2(1., 1.));
    let mut save = false;
    let r = Modal::new(Id::unique("image_viewer")).frame(Frame::NONE).backdrop_color(Color32::from_black_alpha(230)).show(ctx, |ui| {
        ui.with_layout(Layout::top_down(Align::Max), |ui| {
            save = ui.add(primary_button(&p, tr(lang, Key::SaveFile))).clicked();
            ui.add_space(8.);
            match thumb {
                Thumb::Ready(tex, [w, h]) => {
                    let src = vec2(w as f32, h as f32);
                    let (rect, resp) = ui.allocate_exact_size(fit(src, room.min(src)), Sense::hover());
                    Image::new((tex.id(), rect.size())).paint_at(ui, rect);
                    resp.widget_info(|| WidgetInfo::labeled(Role::Image, true, tr(lang, Key::Image)));
                }
                Thumb::Loading => {
                    ui.spinner();
                }
                Thumb::Failed => {
                    ui.label(RichText::new(tr(lang, Key::MediaFileNotFound)).color(Color32::WHITE));
                }
            }
        });
    });
    if save {
        media::save_as(&app.worker, lang, path.clone(), name);
    }
    if r.should_close() {
        app.state.viewer = None;
        app.state.thumbs.forget(&path, VIEW_PX);
    }
}
