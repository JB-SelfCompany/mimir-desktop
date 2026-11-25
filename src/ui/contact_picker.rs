//! Single-select contact list in a dialog (Android ContactSelectorActivity): a search box and one row per contact.
use crate::{
    i18n::{tr, Key, Lang},
    media::{avatar::resolve, thumbs::ThumbCache},
    model::chat_list::{display_name, SAVED_ID},
    ui::{
        avatar::{self, avatar_color},
        theme::{click_row, dialog, dialog_title},
    },
};
use eframe::egui::{self, Align, Id, Label, Layout, ScrollArea, TextEdit};
use mimir::MimirApp;
use std::path::Path;

pub struct ContactPicker {
    /// (public key, shown name, stored avatar path), by name.
    pub contacts: Vec<(Vec<u8>, String, String)>,
    pub query: String,
}

pub enum Pick {
    Contact { pubkey: Vec<u8>, name: String },
    Cancel,
}

impl ContactPicker {
    /// Our contacts without Saved Messages and without the keys in `exclude`.
    pub fn new(core: &MimirApp, exclude: &[Vec<u8>]) -> ContactPicker {
        let mut contacts: Vec<_> = core
            .get_contacts()
            .into_iter()
            .filter(|c| c.id != SAVED_ID && !exclude.contains(&c.pubkey))
            .map(|c| (c.pubkey.clone(), display_name(&c.name, &c.pubkey), c.avatar.clone()))
            .collect();
        contacts.sort_by_key(|c| c.1.to_lowercase());
        ContactPicker { contacts, query: String::new() }
    }

    /// Draws the dialog; `Some` once a contact is clicked or the dialog is dismissed.
    pub fn show(&mut self, ctx: &egui::Context, thumbs: &mut ThumbCache, data_dir: &Path, id: &str, title: &str, lang: Lang) -> Option<Pick> {
        let (mut picked, mut cancel) = (None, false);
        let r = dialog(ctx, Id::NULL.with(id)).show(ctx, |ui| {
            ui.set_width(300.);
            dialog_title(ui, title);
            ui.add(TextEdit::singleline(&mut self.query).hint_text(tr(lang, Key::Search)).desired_width(f32::INFINITY));
            let q = self.query.to_lowercase();
            ScrollArea::vertical().max_height(300.).show(ui, |ui| {
                for (pubkey, name, stored) in self.contacts.iter().filter(|c| c.1.to_lowercase().contains(&q)) {
                    let w = ui.available_width();
                    let row = click_row(ui, |ui| {
                        ui.set_min_width(w);
                        ui.horizontal(|ui| {
                            avatar::show(ui, thumbs, resolve(data_dir, stored).as_deref(), avatar_color(pubkey), 32., 3.);
                            ui.add(Label::new(name).selectable(false));
                        });
                    });
                    if row.response.clicked() {
                        picked = Some(Pick::Contact { pubkey: pubkey.clone(), name: name.clone() });
                    }
                }
            });
            ui.add_space(8.);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| cancel = ui.button(tr(lang, Key::Cancel)).clicked());
        });
        picked.or((cancel || r.should_close()).then_some(Pick::Cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_app;

    #[test]
    fn lists_contacts_without_saved_and_excluded() {
        let (_t, core) = test_app();
        core.add_contact(vec![1; 32], "Борис".into()).unwrap();
        core.add_contact(vec![2; 32], "аня".into()).unwrap();
        core.add_contact(vec![3; 32], "".into()).unwrap();
        let p = ContactPicker::new(&core, &[vec![1; 32]]);
        let names: Vec<_> = p.contacts.iter().map(|c| c.1.as_str()).collect();
        assert_eq!(names, ["03030303", "аня"], "sorted, nameless by key, member excluded, no Saved Messages");
    }
}
