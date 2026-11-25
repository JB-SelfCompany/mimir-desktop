use eframe::egui;
use mimir::{AppEventListener, CallStatus, GroupCallParticipant};
use std::sync::mpsc::Sender;

use super::worker::WorkResult;

/// One variant per `AppEventListener` callback (mimir 0.3.5), plus the worker's results.
#[derive(Debug)]
pub enum Event {
    ConnectivityChanged { is_online: bool },
    MessageReceived { contact_id: i64, message_id: i64, msg_type: i32, reply_to: i64 },
    MessageDelivered { contact_pubkey: Vec<u8>, guid: i64 },
    ContactConnected { pubkey: Vec<u8> },
    ContactDisconnected { pubkey: Vec<u8>, dead: bool },
    GroupMessageReceived { chat_id: i64, message_id: i64, msg_type: i32, reply_to: i64 },
    GroupMessageDeleted { chat_id: i64, guid: i64 },
    GroupMessageDelivered { chat_id: i64, guid: i64 },
    GroupHistorySynced { chat_id: i64 },
    OutgoingMessageStored { is_group: bool, target_id: i64, message_id: i64 },
    MessagesSynced { contact_id: i64 },
    DraftSynced { chat_type: i32, chat_id: i64 },
    GroupChatStatus { chat_id: i64, status: String },
    GroupInviteReceived { invite_id: i64, chat_id: i64, from_pubkey: Vec<u8> },
    MediatorInvitation { from_pubkey: Vec<u8>, mediator_pubkey: Vec<u8> },
    GroupMembersChanged { chat_id: i64 },
    GroupMemberOnline { chat_id: i64, pubkey: Vec<u8>, online: bool },
    ChatStats { chat_id: i64, subscribers: u32, online: u32 },
    MediatorAuthenticated { mediator_pubkey: Vec<u8> },
    MediatorDisconnected { mediator_pubkey: Vec<u8> },
    IncomingCall { pubkey: Vec<u8> },
    CallStatusChanged { status: CallStatus, pubkey: Option<Vec<u8>> },
    CallPacket { pubkey: Vec<u8>, data: Vec<u8> },
    GroupCallJoined { session_id: Vec<u8> },
    GroupCallParticipants { session_id: Vec<u8>, participants: Vec<GroupCallParticipant> },
    GroupCallParticipantEvent { session_id: Vec<u8>, event: u8, pubkey: Vec<u8>, display_name: String },
    GroupCallAudio { session_id: Vec<u8>, from_pubkey: Vec<u8>, seq: u32, aac_frame: Vec<u8> },
    GroupCallError { code: u8, message: String },
    GroupCallDisconnected,
    FileProgress { guid: i64, bytes_done: i64, total_bytes: i64, is_upload: bool },
    FileReceived { pubkey: Vec<u8>, guid: i64, meta_json: String, file_path: String },
    FileDownloaded { chat_id: i64, guid: i64, file_path: String },
    ContactRequestReceived { request_id: i64 },
    ContactResponse { pubkey: Vec<u8>, accepted: bool },
    TrackerAnnounce { ok: bool, ttl: i32 },
    WorkDone(WorkResult),
}

/// Forwards core callbacks (tokio worker threads) into the UI channel and wakes the UI.
pub struct Bridge {
    tx: Sender<Event>,
    ctx: egui::Context,
}

impl Bridge {
    pub fn new(tx: Sender<Event>, ctx: egui::Context) -> Bridge {
        Bridge { tx, ctx }
    }

    fn send(&self, e: Event) {
        // A closed receiver means shutdown; ignore.
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

impl AppEventListener for Bridge {
    fn on_connectivity_changed(&self, is_online: bool) {
        self.send(Event::ConnectivityChanged { is_online });
    }
    fn on_message_received(&self, contact_id: i64, message_id: i64, msg_type: i32, reply_to: i64) {
        self.send(Event::MessageReceived { contact_id, message_id, msg_type, reply_to });
    }
    fn on_message_delivered(&self, contact_pubkey: Vec<u8>, guid: i64) {
        self.send(Event::MessageDelivered { contact_pubkey, guid });
    }
    fn on_contact_connected(&self, pubkey: Vec<u8>) {
        self.send(Event::ContactConnected { pubkey });
    }
    fn on_contact_disconnected(&self, pubkey: Vec<u8>, dead: bool) {
        self.send(Event::ContactDisconnected { pubkey, dead });
    }
    fn on_group_message_received(&self, chat_id: i64, message_id: i64, msg_type: i32, reply_to: i64) {
        self.send(Event::GroupMessageReceived { chat_id, message_id, msg_type, reply_to });
    }
    fn on_group_message_deleted(&self, chat_id: i64, guid: i64) {
        self.send(Event::GroupMessageDeleted { chat_id, guid });
    }
    fn on_group_message_delivered(&self, chat_id: i64, guid: i64) {
        self.send(Event::GroupMessageDelivered { chat_id, guid });
    }
    fn on_group_history_synced(&self, chat_id: i64) {
        self.send(Event::GroupHistorySynced { chat_id });
    }
    fn on_outgoing_message_stored(&self, is_group: bool, target_id: i64, message_id: i64) {
        self.send(Event::OutgoingMessageStored { is_group, target_id, message_id });
    }
    fn on_messages_synced(&self, contact_id: i64) {
        self.send(Event::MessagesSynced { contact_id });
    }
    fn on_draft_synced(&self, chat_type: i32, chat_id: i64) {
        self.send(Event::DraftSynced { chat_type, chat_id });
    }
    fn on_group_chat_status(&self, chat_id: i64, status: String) {
        self.send(Event::GroupChatStatus { chat_id, status });
    }
    fn on_group_invite_received(&self, invite_id: i64, chat_id: i64, from_pubkey: Vec<u8>) {
        self.send(Event::GroupInviteReceived { invite_id, chat_id, from_pubkey });
    }
    fn on_mediator_invitation(&self, from_pubkey: Vec<u8>, mediator_pubkey: Vec<u8>) {
        self.send(Event::MediatorInvitation { from_pubkey, mediator_pubkey });
    }
    fn on_group_members_changed(&self, chat_id: i64) {
        self.send(Event::GroupMembersChanged { chat_id });
    }
    fn on_group_member_online(&self, chat_id: i64, pubkey: Vec<u8>, online: bool) {
        self.send(Event::GroupMemberOnline { chat_id, pubkey, online });
    }
    fn on_chat_stats(&self, chat_id: i64, subscribers: u32, online: u32) {
        self.send(Event::ChatStats { chat_id, subscribers, online });
    }
    fn on_mediator_authenticated(&self, mediator_pubkey: Vec<u8>) {
        self.send(Event::MediatorAuthenticated { mediator_pubkey });
    }
    fn on_mediator_disconnected(&self, mediator_pubkey: Vec<u8>) {
        self.send(Event::MediatorDisconnected { mediator_pubkey });
    }
    fn on_incoming_call(&self, pubkey: Vec<u8>) {
        self.send(Event::IncomingCall { pubkey });
    }
    fn on_call_status_changed(&self, status: CallStatus, pubkey: Option<Vec<u8>>) {
        self.send(Event::CallStatusChanged { status, pubkey });
    }
    fn on_call_packet(&self, pubkey: Vec<u8>, data: Vec<u8>) {
        self.send(Event::CallPacket { pubkey, data });
    }
    fn on_group_call_joined(&self, session_id: Vec<u8>) {
        self.send(Event::GroupCallJoined { session_id });
    }
    fn on_group_call_participants(&self, session_id: Vec<u8>, participants: Vec<GroupCallParticipant>) {
        self.send(Event::GroupCallParticipants { session_id, participants });
    }
    fn on_group_call_participant_event(&self, session_id: Vec<u8>, event: u8, pubkey: Vec<u8>, display_name: String) {
        self.send(Event::GroupCallParticipantEvent { session_id, event, pubkey, display_name });
    }
    fn on_group_call_audio(&self, session_id: Vec<u8>, from_pubkey: Vec<u8>, seq: u32, aac_frame: Vec<u8>) {
        self.send(Event::GroupCallAudio { session_id, from_pubkey, seq, aac_frame });
    }
    fn on_group_call_error(&self, code: u8, message: String) {
        self.send(Event::GroupCallError { code, message });
    }
    fn on_group_call_disconnected(&self) {
        self.send(Event::GroupCallDisconnected);
    }
    fn on_file_progress(&self, guid: i64, bytes_done: i64, total_bytes: i64, is_upload: bool) {
        self.send(Event::FileProgress { guid, bytes_done, total_bytes, is_upload });
    }
    fn on_file_received(&self, pubkey: Vec<u8>, guid: i64, meta_json: String, file_path: String) {
        self.send(Event::FileReceived { pubkey, guid, meta_json, file_path });
    }
    fn on_file_downloaded(&self, chat_id: i64, guid: i64, file_path: String) {
        self.send(Event::FileDownloaded { chat_id, guid, file_path });
    }
    fn on_contact_request_received(&self, request_id: i64) {
        self.send(Event::ContactRequestReceived { request_id });
    }
    fn on_contact_response(&self, pubkey: Vec<u8>, accepted: bool) {
        self.send(Event::ContactResponse { pubkey, accepted });
    }
    fn on_tracker_announce(&self, ok: bool, ttl: i32) {
        self.send(Event::TrackerAnnounce { ok, ttl });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_forwards_and_wakes() {
        let (tx, rx) = std::sync::mpsc::channel();
        let b = Bridge::new(tx, egui::Context::default());
        b.on_message_received(5, 77, 0, 0);
        assert!(matches!(rx.try_recv().unwrap(), Event::MessageReceived { contact_id: 5, message_id: 77, .. }));
    }
}
