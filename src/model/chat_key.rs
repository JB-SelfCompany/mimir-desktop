//! Which chat a row, the selection or an open chat refers to. Contact ids and group chat ids are separate
//! number spaces in the core, so a bare id is ambiguous.
use crate::model::chat_list::SAVED_ID;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ChatKey {
    /// A 1:1 chat; Saved Messages is `Contact(SAVED_ID)`.
    Contact(i64),
    /// A group or channel, by its `chat_id`.
    Group(i64),
}

impl ChatKey {
    pub const SAVED: ChatKey = ChatKey::Contact(SAVED_ID);

    /// `chat_type` of the core's draft calls (CG §1j): 0 for a contact, 1 for a group.
    pub fn draft_type(self) -> i32 {
        match self {
            ChatKey::Contact(_) => 0,
            ChatKey::Group(_) => 1,
        }
    }

    pub fn is_group(self) -> bool {
        matches!(self, ChatKey::Group(_))
    }

    /// The contact id or group chat id, without its kind.
    pub fn id(self) -> i64 {
        match self {
            ChatKey::Contact(id) | ChatKey::Group(id) => id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_type_and_kind() {
        assert_eq!((ChatKey::Contact(5).draft_type(), ChatKey::Group(5).draft_type()), (0, 1));
        assert_ne!(ChatKey::Contact(5), ChatKey::Group(5));
        assert!(ChatKey::Group(5).is_group() && !ChatKey::SAVED.is_group());
        assert_eq!(ChatKey::SAVED.id(), SAVED_ID);
    }
}
