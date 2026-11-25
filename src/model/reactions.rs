//! Reaction chips under a message.
use crate::model::chat_key::ChatKey;
use mimir::MimirApp;

/// (emoji, count, reacted by me) per reaction on message `guid`.
pub fn chips(app: &MimirApp, key: ChatKey, guid: i64) -> Vec<(String, u32, bool)> {
    let rs = match key {
        ChatKey::Contact(id) => app.get_reactions_for_message(id, guid),
        ChatKey::Group(id) => app.get_reactions_for_group_message(id, guid),
    };
    rs.into_iter().map(|r| (r.emoji, r.count, r.user_reacted)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::message::reaction_payload, test_support::test_app};

    #[test] fn reaction_chips_aggregate() {
        let (_t, app) = test_app();
        let c = app.add_contact(vec![1; 32], "A".into()).unwrap();
        app.add_local_message(c, 5, 0, true, true, 1, 0, 0, b"hi".to_vec()).unwrap();
        app.add_local_message(c, 100, 5, true, true, 2, 0, 10, reaction_payload("👍", 5)).unwrap();
        app.add_local_message(c, 101, 5, false, true, 3, 0, 10, reaction_payload("👍", 5)).unwrap();
        assert_eq!(chips(&app, ChatKey::Contact(c), 5), vec![("👍".to_string(), 2, true)]);
    }

    #[test] fn group_reaction_chips_aggregate() {
        let (_t, app) = test_app();
        app.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        app.add_group_message(7, 1, 11, vec![1; 32], 1, 0, false, b"hi".to_vec(), 0, false).unwrap();
        app.add_group_message(7, 2, 12, vec![2; 32], 2, 10, false, reaction_payload("👍", 11), 11, false).unwrap();
        app.add_group_message(7, 3, 13, app.public_key(), 3, 10, false, reaction_payload("👍", 11), 11, false).unwrap();
        assert_eq!(chips(&app, ChatKey::Group(7), 11), vec![("👍".to_string(), 2, true)]);
    }
}
