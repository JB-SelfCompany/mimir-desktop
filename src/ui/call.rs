//! 1:1 calls (Android CallActivity): the dialog for an outgoing, incoming or running call, its ring sounds, the
//! core's call status driving it, and the call record rows it leaves in the chat (type 2, as Android reads them:
//! the duration is `edit_time - send_time`). Group calls (Android GroupCallActivity) on the call mediator: started
//! from a group (its `mimir://gc/…` invite posted there) or joined from such a link, with the participants list.
use crate::{
    app::MimirDesktop,
    core::{events::Event, net, worker::WorkResult},
    i18n::{tr, Key, Lang},
    media::call_audio::AudioError,
    model::{chat_key::ChatKey, chat_list::{build_chat_list, display_name}, send::{send_text, Target}},
    ui::{avatar, theme::{self, Palette}},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use eframe::egui::{self, vec2, Align2, Color32, Frame, Id, Painter, Pos2, RichText, Role, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo};
use mimir::CallStatus;
use std::{
    path::PathBuf,
    sync::{atomic::{AtomicBool, Ordering::SeqCst}, Arc},
    time::{Duration, Instant},
};

const RINGTONE: &[u8] = include_bytes!("../../assets/sounds/ringtone.ogg");
const RINGBACK: &[u8] = include_bytes!("../../assets/sounds/ring_back_tone.ogg");
const ACCEPT: &[u8] = include_bytes!("../../assets/sounds/accept_call.ogg");
const DECLINE: &[u8] = include_bytes!("../../assets/sounds/decline_call.ogg");

/// Android's button_rounded_green / _red.
const GREEN: Color32 = Color32::from_rgb(0x00, 0xC2, 0x00);
const RED: Color32 = Color32::from_rgb(0xC2, 0x00, 0x00);
const BUTTON: f32 = 56.;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// We asked the core to call; it has not reported Calling yet (the contact may still be dialled).
    Dialing,
    /// Calling: the contact's device rings.
    Ringing,
    /// The contact calls us.
    Incoming,
    /// Talking since then.
    Active(Instant),
}

pub struct Call {
    pub pubkey: Vec<u8>,
    /// The contact's row (none: a caller we have no contact for, which leaves no record).
    contact_id: Option<i64>,
    pub name: String,
    avatar: Option<PathBuf>,
    incoming: bool,
    pub phase: Phase,
    /// When it rang or was dialled, and when it was answered (ms since the epoch), for the record.
    started_ms: i64,
    answered_ms: Option<i64>,
    pub muted: bool,
    /// Why the call has no sound, as the dialog says it (`Event::CallAudio`).
    pub audio_error: Option<String>,
    /// `start_call` failed (the contact is not connected): tried again when it connects (Android).
    pending: bool,
    /// The ringtone or ringback while it plays.
    ring: Option<Ring>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Android formatDuration: «01:05», or «01:02:03» past an hour.
pub fn format_duration(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 { format!("{h:02}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// The two lines of a call record row (Android MessageAdapter type 2): what kind of call, and how long it lasted.
/// An incoming call of no length was never answered: missed, without a duration.
pub fn record_text(incoming: bool, send_time: i64, edit_time: i64, lang: Lang) -> (&'static str, String) {
    let ms = edit_time - send_time;
    match (incoming, ms > 0) {
        (true, false) => (tr(lang, Key::MissedCall), String::new()),
        (true, true) => (tr(lang, Key::IncomingCall), format_duration(ms)),
        (false, _) => (tr(lang, Key::OutgoingCall), format_duration(ms)),
    }
}

/// Whether a call can be started now, and the call button's hint (what is missing, if ffmpeg is).
pub fn can_call(app: &MimirDesktop) -> (bool, &'static str) {
    if !app.call_audio.available() {
        return (false, tr(app.lang, Key::InstallFfmpegCalls));
    }
    (app.state.core_started && idle(app), tr(app.lang, Key::MakeCall))
}

/// No call of either kind: one at a time, as in the core.
fn idle(app: &MimirDesktop) -> bool {
    app.state.call.is_none() && app.state.group_call.is_none()
}

fn new_call(app: &MimirDesktop, pubkey: &[u8], incoming: bool) -> Call {
    let contact = app.core.get_contact_by_pubkey(pubkey.to_vec());
    let name = display_name(contact.as_ref().map_or("", |c| c.name.as_str()), pubkey);
    let avatar = contact.as_ref().and_then(|c| crate::media::avatar::resolve(&app.paths.core_dir, &c.avatar));
    let (phase, sound) = if incoming { (Phase::Incoming, RINGTONE) } else { (Phase::Dialing, RINGBACK) };
    Call {
        pubkey: pubkey.to_vec(), contact_id: contact.map(|c| c.id), name, avatar, incoming, phase, started_ms: now_ms(),
        answered_ms: None, muted: false, audio_error: None, pending: false, ring: Some(ring(sound)),
    }
}

/// The header's call button: calls the contact, online or not (Android dials it first and calls once connected).
pub fn start(app: &mut MimirDesktop, pubkey: Vec<u8>) {
    if app.state.call.is_some() {
        return;
    }
    let mut call = new_call(app, &pubkey, false);
    if app.core.start_call(pubkey.clone()).is_err() {
        call.pending = true;
        net::connect(&app.worker, pubkey);
    }
    app.state.call = Some(call);
}

/// The core's ring timeout (30 s) and a margin: a call still «Подключение…» then is given up on (the contact never
/// connected, so the core's own timeout never started).
const DIAL_TIMEOUT_MS: i64 = 40_000;

/// What the core says about calls (and contacts connecting, for a call waiting on one), and the audio's failures.
pub fn on_event(app: &mut MimirDesktop, ctx: &egui::Context, e: &Event) {
    match e {
        Event::IncomingCall { pubkey } => {
            // Busy, or the same call announced again: another call rings on unanswered until its caller gives up.
            // ponytail: the second caller is not told we are busy (Android shows «call already ongoing» only locally).
            if !idle(app) {
                return;
            }
            app.state.call = Some(new_call(app, pubkey, true));
            // Raised from the tray, as Android's call screen comes up over everything.
            crate::platform::handle(app, ctx, &crate::platform::Ext::Show);
        }
        Event::CallStatusChanged { status, pubkey } => {
            let Some(call) = app.state.call.as_mut() else { return };
            if pubkey.as_ref().is_some_and(|k| *k != call.pubkey) {
                return;
            }
            match status {
                CallStatus::Calling if call.phase == Phase::Dialing => call.phase = Phase::Ringing,
                CallStatus::InCall if !matches!(call.phase, Phase::Active(_)) => {
                    call.phase = Phase::Active(Instant::now());
                    call.answered_ms = Some(now_ms());
                    call.ring = None;
                    play_once(ACCEPT);
                    let pubkey = call.pubkey.clone();
                    app.call_audio.start_peer(pubkey);
                }
                CallStatus::Hangup | CallStatus::Idle => end(app),
                _ => {}
            }
        }
        Event::CallAudio(err) => {
            let text = Some(audio_text(err, app.lang));
            if let Some(call) = app.state.call.as_mut() {
                call.audio_error = text;
            } else if let Some(gc) = app.state.group_call.as_mut() {
                gc.audio_error = text;
            }
        }
        Event::ContactConnected { pubkey } => {
            if let Some(call) = app.state.call.as_mut().filter(|c| c.pending && c.pubkey == *pubkey) {
                call.pending = app.core.start_call(pubkey.clone()).is_err();
            }
        }
        _ => on_group_event(app, e),
    }
}

fn audio_text(e: &AudioError, lang: Lang) -> String {
    match e {
        AudioError::Microphone(_) => tr(lang, Key::CallNoMicrophone).to_string(),
        AudioError::Speaker(_) => tr(lang, Key::CallNoSpeaker).to_string(),
        AudioError::Codec(detail) => format!("{}: {detail}", tr(lang, Key::CallError)),
    }
}

/// The call is over (either side, or the network): its sound stops, the decline tone plays, its record is stored.
fn end(app: &mut MimirDesktop) {
    let Some(call) = app.state.call.take() else { return };
    app.call_audio.stop();
    play_once(DECLINE);
    record(app, &call);
}

/// Stores the call's record row and shows it. Local only: each Android device keeps its own records too.
// ponytail: a declined call is stored like a missed one (incoming, no length); the row format has no place to tell.
fn record(app: &mut MimirDesktop, call: &Call) {
    let Some(contact_id) = call.contact_id else { return };
    let send = call.answered_ms.unwrap_or(call.started_ms);
    let edit = if call.answered_ms.is_some() { now_ms() } else { send };
    let guid = app.core.generate_guid(send, vec![]);
    match app.core.add_local_message(contact_id, guid, 0, call.incoming, true, send, edit, 2, vec![]) {
        Ok(id) => {
            if let Some(chat) = app.state.open_chat.as_mut().filter(|c| c.key == ChatKey::Contact(contact_id)) {
                chat.timeline.push_if_newer(app.core.get_message(id));
            }
            app.state.chats = build_chat_list(&app.core, app.lang);
        }
        Err(e) => tracing::warn!("call record not stored: {e}"),
    }
}

/// The app is closing: a call we made or took is hung up (the contact would hear silence otherwise). One that still
/// rings is left alone: nobody acted on it, and another device of ours may answer it.
pub fn on_exit(app: &mut MimirDesktop) {
    if app.state.call.as_ref().is_some_and(|c| c.phase != Phase::Incoming) {
        hang_up(app);
    }
    if app.state.group_call.is_some() {
        end_group(app);
        app.core.leave_group_call();
    }
}

fn hang_up(app: &mut MimirDesktop) {
    let Some(call) = app.state.call.as_ref() else { return };
    let _ = app.core.hangup_call_with(call.pubkey.clone());
    end(app);
}

#[derive(Clone, Copy)]
enum Click {
    Answer,
    Decline,
    HangUp,
    Mute,
}

#[derive(Clone, Copy)]
enum Icon {
    Phone,
    HangUp,
    /// Crossed out while muted.
    Mic(bool),
}

/// The call dialog: modal while a call rings for us (Android's full-screen call screen), otherwise a small window in
/// the top-right corner, so the chats stay usable while calling.
pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    show_peer(app, ctx);
    show_group(app, ctx);
    join_dialog(app, ctx);
}

fn show_peer(app: &mut MimirDesktop, ctx: &egui::Context) {
    if app.state.call.as_ref().is_some_and(|c| c.phase == Phase::Dialing && now_ms() - c.started_ms > DIAL_TIMEOUT_MS) {
        app.error_text(tr(app.lang, Key::CallUnreachable).into());
        return hang_up(app);
    }
    let Some(call) = app.state.call.as_ref() else { return };
    let (lang, can_answer) = (app.lang, app.call_audio.available());
    let p = theme::palette(ctx.global_style().visuals.dark_mode);
    let thumbs = &mut app.state.thumbs;
    let mut click = None;
    if call.phase == Phase::Incoming {
        theme::dialog(ctx, Id::unique("call")).show(ctx, |ui| click = contents(ui, call, thumbs, can_answer, lang, &p));
    } else {
        egui::Window::new("call")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::RIGHT_TOP, vec2(-16., 56.))
            .frame(Frame::popup(&ctx.global_style()).inner_margin(16))
            .show(ctx, |ui| click = contents(ui, call, thumbs, can_answer, lang, &p));
    }
    // The timer, and the dial timeout.
    if call.phase != Phase::Incoming {
        ctx.request_repaint_after(Duration::from_secs(1));
    }
    let pubkey = call.pubkey.clone();
    match click {
        // The core reports InCall next, which starts the audio.
        Some(Click::Answer) => {
            if let Err(e) = app.core.answer_call(pubkey.clone(), true) {
                app.error_text(format!("{}: {e}", tr(lang, Key::CallError)));
                // Declined, so the caller stops ringing (when the core can still reach them).
                let _ = app.core.answer_call(pubkey, false);
                end(app);
            }
        }
        // Android's call_decline: answer_call(false).
        Some(Click::Decline) => {
            let _ = app.core.answer_call(pubkey, false);
            end(app);
        }
        Some(Click::HangUp) => hang_up(app),
        Some(Click::Mute) => {
            if let Some(call) = app.state.call.as_mut() {
                call.muted = !call.muted;
                app.call_audio.set_muted(call.muted);
            }
        }
        None => {}
    }
}

fn contents(ui: &mut Ui, call: &Call, thumbs: &mut crate::media::thumbs::ThumbCache, can_answer: bool, lang: Lang, p: &Palette) -> Option<Click> {
    ui.set_width(260.);
    let mut click = None;
    ui.vertical_centered(|ui| {
        // incoming_call_activity: a 128 px rounded avatar, the name (20 sp), the status or timer (18 sp).
        avatar::show(ui, thumbs, call.avatar.as_deref(), avatar::avatar_color(&call.pubkey), 128., 3.);
        ui.add_space(8.);
        ui.add(egui::Label::new(RichText::new(&call.name).size(20.).color(p.text)).selectable(false));
        let status = match call.phase {
            Phase::Dialing => tr(lang, Key::CallConnecting).to_string(),
            Phase::Ringing => tr(lang, Key::CallRinging).to_string(),
            Phase::Incoming => tr(lang, Key::IncomingCall).to_string(),
            Phase::Active(since) => format_duration(since.elapsed().as_millis() as i64),
        };
        ui.add(egui::Label::new(RichText::new(status).size(18.).color(p.text_dim)).selectable(false));
        if let Some(e) = &call.audio_error {
            ui.add(egui::Label::new(RichText::new(e).size(13.).color(p.danger)).selectable(false));
        }
        ui.add_space(16.);
        let buttons = match call.phase {
            Phase::Incoming => vec![
                (Click::Answer, GREEN, tr(lang, if can_answer { Key::CallAnswer } else { Key::InstallFfmpegCalls }), can_answer, Icon::Phone),
                (Click::Decline, RED, tr(lang, Key::CallDecline), true, Icon::HangUp),
            ],
            Phase::Dialing | Phase::Ringing => vec![(Click::HangUp, RED, tr(lang, Key::Cancel), true, Icon::HangUp)],
            Phase::Active(_) => vec![
                (Click::Mute, p.file_icon, tr(lang, if call.muted { Key::CallUnmute } else { Key::CallMute }), true, Icon::Mic(call.muted)),
                (Click::HangUp, RED, tr(lang, Key::CallHangUp), true, Icon::HangUp),
            ],
        };
        click = button_row(ui, buttons, p);
    });
    click
}

/// The dialog's round buttons, centred: (what, fill, label, enabled, icon) each; the one clicked.
fn button_row(ui: &mut Ui, buttons: Vec<(Click, Color32, &str, bool, Icon)>, p: &Palette) -> Option<Click> {
    let gap = 32.;
    let n = buttons.len() as f32;
    let row = n * BUTTON + (n - 1.) * gap;
    let mut click = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.add_space(((ui.available_width() - row) / 2.).max(0.));
        for (what, fill, label, enabled, icon) in buttons {
            if round_button(ui, fill, label, enabled, icon, p) {
                click = Some(what);
            }
        }
    });
    click
}

/// A round 56 px button with an icon; `label` is its tooltip and accessible name.
fn round_button(ui: &mut Ui, fill: Color32, label: &str, enabled: bool, icon: Icon, p: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BUTTON), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    let fill = match (enabled, resp.hovered()) {
        (false, _) => fill.gamma_multiply(0.4),
        (true, true) => fill.gamma_multiply(0.85),
        (true, false) => fill,
    };
    ui.painter().circle_filled(rect.center(), BUTTON / 2., fill);
    match icon {
        Icon::Phone => phone_icon(ui, rect.center(), 24., Color32::WHITE, false),
        Icon::HangUp => phone_icon(ui, rect.center(), 24., Color32::WHITE, true),
        Icon::Mic(muted) => mic_icon(ui.painter(), rect.center(), p.text, muted),
    }
    let resp = resp.on_hover_text(label);
    enabled && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// The MDI handset (ic_phone_outline) of `size` px, tinted `color`; `hang_up`: ic_phone_hangup_outline, for
/// declining and hanging up.
pub fn phone_icon(ui: &Ui, c: Pos2, size: f32, color: Color32, hang_up: bool) {
    let icon = if hang_up {
        egui::include_image!("../../assets/icons/phone-hangup-outline.svg")
    } else {
        egui::include_image!("../../assets/icons/phone-outline.svg")
    };
    egui::Image::new(icon).tint(color).paint_at(ui, egui::Rect::from_center_size(c, Vec2::splat(size)));
}

/// A microphone (ic_microphone_outline), crossed out when `off` (ic_microphone_off).
fn mic_icon(painter: &Painter, c: Pos2, color: Color32, off: bool) {
    let s = Stroke::new(2., color);
    painter.rect_stroke(egui::Rect::from_center_size(c + vec2(0., -4.), vec2(8., 14.)), 4, s, egui::StrokeKind::Middle);
    let arc: Vec<Pos2> = (0..=12).map(|i| c + vec2(0., -1.) + 8. * Vec2::angled(std::f32::consts::PI * i as f32 / 12.)).collect();
    painter.add(Shape::line(arc, s));
    painter.line_segment([c + vec2(0., 7.), c + vec2(0., 11.)], s);
    if off {
        painter.line_segment([c + vec2(-9., -10.), c + vec2(9., 10.)], s);
    }
}

// ---------------------------------------------------------------------------------------- group calls

/// Android GroupCallActivity's speaking dot: green while heard, grey otherwise.
const SPEAKING: Color32 = Color32::from_rgb(0x44, 0xCC, 0x44);
const SILENT: Color32 = Color32::from_rgb(0x99, 0x99, 0x99);

/// A group call on the call mediator: being created or joined, then running.
pub struct GroupCall {
    /// This attempt's create/join: its result carries the same token, and a result of an abandoned attempt is dropped.
    token: u64,
    /// The group it was started from: the invite is posted there once the session exists. None when joined by link.
    chat_id: Option<i64>,
    /// Joined since; None while the session is created or joined.
    pub since: Option<Instant>,
    /// The roster the call mediator reports (us included): pubkey and display name.
    pub participants: Vec<(Vec<u8>, String)>,
    pub muted: bool,
    /// Why the call has no sound (`Event::CallAudio`), as the 1:1 dialog says it.
    pub audio_error: Option<String>,
}

static TOKENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_token() -> u64 {
    TOKENS.fetch_add(1, SeqCst)
}

impl GroupCall {
    fn new(token: u64, chat_id: Option<i64>) -> GroupCall {
        GroupCall { token, chat_id, since: None, participants: vec![], muted: false, audio_error: None }
    }
}

/// A group call invite: `mimir://gc/<mediator hex>/<session id base32>#<session key base64url>` (Android
/// InviteLinkCodec; the key rides in the fragment).
#[derive(Debug, PartialEq)]
pub struct Invite {
    pub mediator: Vec<u8>,
    pub session_id: Vec<u8>,
    pub key: Vec<u8>,
}

/// RFC 4648 base32, no padding.
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Android writes the session id in upper case.
pub fn invite_link(i: &Invite) -> String {
    let (mut sid, mut bits, mut n) = (String::new(), 0u32, 0);
    for &b in &i.session_id {
        (bits, n) = (bits << 8 | b as u32, n + 8);
        while n >= 5 {
            n -= 5;
            sid.push(B32[(bits >> n & 31) as usize] as char);
        }
    }
    if n > 0 {
        sid.push(B32[(bits << (5 - n) & 31) as usize] as char);
    }
    format!("mimir://gc/{}/{sid}#{}", hex::encode(&i.mediator), URL_SAFE_NO_PAD.encode(&i.key))
}

/// An SFU invite (with its key) of the right sizes; the base32 is read in either case. Anything else is `None`.
pub fn parse_invite(link: &str) -> Option<Invite> {
    let (path, key) = link.trim().strip_prefix("mimir://gc/")?.split_once('#')?;
    let mut parts = path.trim_matches('/').split('/');
    let mediator = hex::decode(parts.next()?).ok().filter(|k| k.len() == 32)?;
    let (mut session_id, mut bits, mut n) = (vec![], 0u32, 0);
    for c in parts.next()?.bytes() {
        let v = B32.iter().position(|&x| x == c.to_ascii_uppercase())? as u32;
        (bits, n) = (bits << 5 | v, n + 5);
        if n >= 8 {
            n -= 8;
            session_id.push((bits >> n) as u8);
        }
    }
    let key = URL_SAFE_NO_PAD.decode(key.trim_end_matches('=')).ok().filter(|k| k.len() == 32)?;
    (session_id.len() == 16).then_some(Invite { mediator, session_id, key })
}

/// The group header's call button: (enabled, hint). Without a call mediator it stays clickable and says where to set
/// one (Android's snackbar).
pub fn can_group_call(app: &MimirDesktop) -> (bool, &'static str) {
    if !app.call_audio.available() {
        return (false, tr(app.lang, Key::InstallFfmpegCalls));
    }
    if app.settings.call_mediator.is_empty() {
        return (true, tr(app.lang, Key::GroupCallNeedsMediator));
    }
    (app.state.core_started && idle(app), tr(app.lang, Key::StartGroupCall))
}

/// Our name in the call: the profile's (Android fills the pre-join field with it), at most 64 characters.
fn call_name(app: &MimirDesktop) -> String {
    app.core.get_account(crate::ui::profile::ACCOUNT).map(|a| a.name.trim().chars().take(64).collect()).unwrap_or_default()
}

/// «Новый групповой звонок» in group `chat_id`: a new session on the call mediator; its invite goes into the group.
pub fn start_group(app: &mut MimirDesktop, chat_id: i64) {
    if app.settings.call_mediator.is_empty() {
        return app.error_text(tr(app.lang, Key::GroupCallNeedsMediator).into());
    }
    if !idle(app) {
        return;
    }
    // Settings keep only a valid 64-digit key.
    let mediator = hex::decode(&app.settings.call_mediator).unwrap_or_default();
    let (name, token) = (call_name(app), next_token());
    app.state.group_call = Some(GroupCall::new(token, Some(chat_id)));
    // Mode 0: SFU, end-to-end encrypted (Android; MCU is not wired up there either).
    app.worker.submit(Box::new(move |core| match core.create_group_call(mediator.clone(), 0, name, vec![]) {
        Ok(s) => WorkResult::GroupCallUp { token, link: invite_link(&Invite { mediator, session_id: s.session_id, key: s.session_key }) },
        Err(e) => WorkResult::GroupCallFailed { token, err: e.to_string() },
    }));
}

/// A `mimir://gc/…` link (OS or chat bubble): asks before joining.
pub fn ask_join(app: &mut MimirDesktop, link: &str) {
    if parse_invite(link).is_none() {
        app.error_text(tr(app.lang, Key::GroupCallInvalidLink).into());
    } else if !idle(app) {
        app.error_text(tr(app.lang, Key::CallAlreadyOngoing).into());
    } else {
        app.state.join_group_call = Some(link.trim().to_string());
    }
}

fn join(app: &mut MimirDesktop, link: String) {
    let Some(i) = parse_invite(&link).filter(|_| idle(app)) else { return };
    let (name, token) = (call_name(app), next_token());
    app.state.group_call = Some(GroupCall::new(token, None));
    app.worker.submit(Box::new(move |core| match core.join_group_call(i.mediator, i.session_id, i.key, name, vec![]) {
        Ok(()) => WorkResult::GroupCallUp { token, link },
        Err(e) => WorkResult::GroupCallFailed { token, err: e.to_string() },
    }));
}

/// We are in the session: the audio starts (Android starts it here, not on the joined event, which may come first),
/// and a call started from a group posts its invite there as a text message.
pub fn group_up(app: &mut MimirDesktop, token: u64, link: &str) {
    let Some(gc) = app.state.group_call.as_mut().filter(|g| g.token == token) else {
        // An abandoned attempt got in after all. With no call now, nothing else would end its session; a newer
        // attempt's create/join replaces it in the core (a new connection tears the old session down first), and a
        // leave queued behind that would end the new call instead.
        if app.state.group_call.is_none() {
            queue_leave(app);
        }
        return;
    };
    gc.since = Some(Instant::now());
    let (chat, muted) = (gc.chat_id, gc.muted);
    app.call_audio.set_muted(muted);
    app.call_audio.start_group();
    play_once(ACCEPT);
    if let Some(chat_id) = chat {
        send_text(app.worker_for(ChatKey::Group(chat_id)), Target::Group { chat_id }, 0, link.to_string());
    }
}

/// Creating or joining failed (no route to the call mediator, a bad session).
pub fn group_failed(app: &mut MimirDesktop, token: u64, err: &str) {
    if app.state.group_call.take_if(|g| g.token == token).is_some() {
        app.error_text(format!("{}: {err}", tr(app.lang, Key::CallError)));
    }
}

/// The call is over for us: its sound stops and the decline tone plays.
fn end_group(app: &mut MimirDesktop) {
    if app.state.group_call.take().is_some() {
        app.call_audio.stop();
        play_once(DECLINE);
    }
}

/// «Выйти»: off the UI thread, behind a create/join still running.
fn leave_group(app: &mut MimirDesktop) {
    end_group(app);
    queue_leave(app);
}

fn queue_leave(app: &MimirDesktop) {
    app.worker.submit(Box::new(|core| {
        core.leave_group_call();
        WorkResult::Done
    }));
}

/// The call mediator's roster and errors.
fn on_group_event(app: &mut MimirDesktop, e: &Event) {
    let Some(gc) = app.state.group_call.as_mut() else { return };
    match e {
        Event::GroupCallParticipants { participants, .. } => {
            gc.participants = participants.iter().map(|p| (p.pubkey.clone(), p.display_name.clone())).collect();
        }
        // EVT_JOINED / EVT_LEFT.
        Event::GroupCallParticipantEvent { event: 1, pubkey, display_name, .. } => {
            if !gc.participants.iter().any(|(k, _)| k == pubkey) {
                gc.participants.push((pubkey.clone(), display_name.clone()));
            }
        }
        Event::GroupCallParticipantEvent { event: 2, pubkey, .. } => gc.participants.retain(|(k, _)| k != pubkey),
        // Android shows the message and closes the call screen.
        Event::GroupCallError { message, .. } => {
            let text = if message.is_empty() { tr(app.lang, Key::CallError).to_string() } else { message.clone() };
            app.error_text(text);
            end_group(app);
        }
        Event::GroupCallDisconnected => end_group(app),
        _ => {}
    }
}

/// The group call window, where the 1:1 one goes: title, timer, participants with their speaking dots, mute, «Выйти».
fn show_group(app: &mut MimirDesktop, ctx: &egui::Context) {
    let Some(gc) = app.state.group_call.as_ref() else { return };
    let (lang, me) = (app.lang, app.core.public_key());
    let p = theme::palette(ctx.global_style().visuals.dark_mode);
    let (heard, me_heard) = (app.call_audio.speaking(), app.call_audio.mic_voiced());
    let mut click = None;
    egui::Window::new("group_call")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(Align2::RIGHT_TOP, vec2(-16., 56.))
        .frame(Frame::popup(&ctx.global_style()).inner_margin(16))
        .show(ctx, |ui| {
            ui.set_width(260.);
            ui.vertical_centered(|ui| {
                ui.add(egui::Label::new(RichText::new(tr(lang, Key::GroupCall)).size(20.).color(p.text)).selectable(false));
                let status = gc.since.map_or_else(|| tr(lang, Key::CallConnecting).to_string(), |t| format_duration(t.elapsed().as_millis() as i64));
                ui.add(egui::Label::new(RichText::new(status).size(18.).color(p.text_dim)).selectable(false));
                if let Some(e) = &gc.audio_error {
                    ui.add(egui::Label::new(RichText::new(e).size(13.).color(p.danger)).selectable(false));
                }
            });
            ui.add_space(12.);
            for (pubkey, name) in &gc.participants {
                ui.horizontal(|ui| {
                    let ours = *pubkey == me;
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.), Sense::hover());
                    let speaking = if ours { me_heard } else { heard.contains(pubkey) };
                    ui.painter().circle_filled(dot.center(), 5., if speaking { SPEAKING } else { SILENT });
                    let mut text = if name.trim().is_empty() { hex::encode(pubkey)[..8].to_string() } else { name.clone() };
                    if ours {
                        text = format!("{text} ({})", tr(lang, Key::GroupCallYou));
                    }
                    ui.add(egui::Label::new(RichText::new(text).color(p.text)).selectable(false));
                });
            }
            ui.add_space(16.);
            let mute = tr(lang, if gc.muted { Key::CallUnmute } else { Key::CallMute });
            let buttons = vec![
                (Click::Mute, p.file_icon, mute, true, Icon::Mic(gc.muted)),
                (Click::HangUp, RED, tr(lang, Key::GroupCallLeave), true, Icon::HangUp),
            ];
            click = button_row(ui, buttons, &p);
        });
    // The timer and the speaking dots (Android ticks them every 200 ms).
    ctx.request_repaint_after(Duration::from_millis(200));
    match click {
        Some(Click::HangUp) => leave_group(app),
        Some(Click::Mute) => {
            if let Some(gc) = app.state.group_call.as_mut() {
                gc.muted = !gc.muted;
                app.call_audio.set_muted(gc.muted);
            }
        }
        _ => {}
    }
}

/// «Войти в групповой звонок?» for a link that was opened.
fn join_dialog(app: &mut MimirDesktop, ctx: &egui::Context) {
    let Some(link) = app.state.join_group_call.clone() else { return };
    let (lang, can) = (app.lang, app.call_audio.available());
    let p = theme::palette(ctx.global_style().visuals.dark_mode);
    let (mut yes, mut no) = (false, false);
    let r = theme::dialog(ctx, Id::unique("join_group_call")).show(ctx, |ui| {
        ui.set_width(260.);
        theme::dialog_title(ui, tr(lang, Key::GroupCall));
        ui.label(tr(lang, if can { Key::GroupCallJoinQuestion } else { Key::InstallFfmpegCalls }));
        ui.add_space(12.);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            yes = ui.add_enabled(can, theme::primary_button(&p, tr(lang, Key::GroupCallJoin))).clicked();
            no = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if yes || no || r.should_close() {
        app.state.join_group_call = None;
    }
    if yes {
        join(app, link);
    }
}

/// A ring sound looping until dropped.
struct Ring(Arc<AtomicBool>);

impl Drop for Ring {
    fn drop(&mut self) {
        self.0.store(true, SeqCst);
    }
}

fn ring(sound: &'static [u8]) -> Ring {
    let stop = Arc::new(AtomicBool::new(false));
    // Tests never play sounds.
    if !cfg!(test) {
        let s = stop.clone();
        std::thread::spawn(move || play(sound, Some(s)));
    }
    Ring(stop)
}

fn play_once(sound: &'static [u8]) {
    if !cfg!(test) {
        std::thread::spawn(move || play(sound, None));
    }
}

/// Plays `sound` (ogg) on a short-lived output stream: once, or over and over until `stop` is set. No output device
/// only means silence.
fn play(sound: &'static [u8], stop: Option<Arc<AtomicBool>>) {
    crate::media::pin_audio_enumerator();
    let Ok((_stream, out)) = rodio::OutputStream::try_default() else { return };
    let Ok(sink) = rodio::Sink::try_new(&out) else { return };
    loop {
        if sink.empty() {
            match crate::media::player::embedded(sound, "ogg") {
                Ok(s) => sink.append(s),
                Err(e) => return tracing::warn!("call sound: {e}"),
            }
        }
        let Some(stop) = &stop else { return sink.sleep_until_end() };
        // Dropping the sink stops the sound.
        if stop.load(SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{media::call_audio::CallAudio, model::chat_key::ChatKey, settings::Settings, test_support::test_harness};
    use egui_kittest::{kittest::{NodeT as _, Queryable as _}, Harness};

    const ANYA: [u8; 32] = [7; 32];

    /// Contact «Аня»'s chat open, the core «started», and an engine that has (a stand-in for) ffmpeg when `ffmpeg`.
    /// The engine reports into a channel nobody reads: the tests send the events themselves.
    fn chat(ffmpeg: bool) -> Harness<'static, MimirDesktop> {
        chat_with(ffmpeg, |_, _| {})
    }

    /// `chat`, with `setup` (core, contact id) run before it opens.
    fn chat_with(ffmpeg: bool, setup: impl FnOnce(&mimir::MimirApp, i64)) -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(ANYA.to_vec(), "Аня".into()).unwrap();
        setup(&core, c);
        let app = h.state_mut();
        app.state.core_started = true;
        app.state.chats = build_chat_list(&core, Lang::Ru);
        app.state.selected = Some(ChatKey::Contact(c));
        let bin = ffmpeg.then(|| PathBuf::from("ffmpeg-for-tests"));
        app.call_audio = CallAudio::new(core.clone(), bin, std::sync::mpsc::channel().0, egui::Context::default());
        // Jobs (dials, group call create/join/leave) report nowhere: the tests send the results themselves.
        app.worker = crate::core::worker::Worker::spawn(core, std::sync::mpsc::channel().0, egui::Context::default());
        h.run();
        h
    }

    fn send(h: &mut Harness<'static, MimirDesktop>, e: Event) {
        h.state().events.send(e).unwrap();
        h.run();
    }

    fn status(status: CallStatus) -> Event {
        Event::CallStatusChanged { status, pubkey: Some(ANYA.to_vec()) }
    }

    fn records(h: &Harness<'static, MimirDesktop>) -> Vec<(bool, i64)> {
        let chat = h.state().state.open_chat.as_ref().unwrap();
        chat.timeline.msgs.iter().filter(|m| m.msg_type == 2).map(|m| (m.incoming, m.edit_time - m.send_time)).collect()
    }

    fn shown(h: &Harness<'static, MimirDesktop>, label: &str) -> bool {
        h.query_by_label(label).is_some()
    }

    #[test]
    fn durations_like_android() {
        assert_eq!(format_duration(0), "00:00");
        assert_eq!(format_duration(65_400), "01:05");
        assert_eq!(format_duration(3_723_000), "01:02:03");
        assert_eq!(record_text(true, 1000, 43_000, Lang::Ru), ("Входящий звонок", "00:42".to_string()));
        assert_eq!(record_text(false, 1000, 1000, Lang::En), ("Outgoing audio call", "00:00".to_string()));
        assert_eq!(record_text(true, 1000, 1000, Lang::Ru), ("Пропущенный звонок", String::new()), "never answered");
        assert_eq!(record_text(true, 1000, 0, Lang::Ru).0, "Пропущенный звонок", "a row without an end time");
    }

    #[test]
    fn call_sounds_decode() {
        for s in [RINGTONE, RINGBACK, ACCEPT, DECLINE] {
            assert!(crate::media::player::embedded(s, "ogg").is_ok());
        }
    }

    #[test]
    fn call_button_needs_ffmpeg_and_a_contact() {
        let mut h = chat(false);
        assert!(h.get_by_label("Позвонить").accesskit_node().is_disabled(), "no ffmpeg: no calls");
        h.get_by_label("Позвонить").click();
        h.run();
        assert!(h.state().state.call.is_none());
        let mut h = chat(true);
        assert!(!h.get_by_label("Позвонить").accesskit_node().is_disabled(), "offline contacts can be called");
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        assert!(h.get_by_label("Позвонить").accesskit_node().is_disabled(), "Saved Messages");
    }

    #[test]
    fn outgoing_call_rings_then_is_cancelled() {
        let mut h = chat(true);
        h.get_by_label("Позвонить").click();
        h.run();
        assert!(shown(&h, "Подключение…"), "dialled first");
        assert!(h.get_by_label("Позвонить").accesskit_node().is_disabled(), "one call at a time");
        send(&mut h, Event::CallStatusChanged { status: CallStatus::Calling, pubkey: Some(vec![8; 32]) });
        assert!(shown(&h, "Подключение…"), "another contact's status");
        send(&mut h, status(CallStatus::Calling));
        assert!(shown(&h, "Вызов…"));
        h.get_by_label("Отмена").click();
        h.run();
        assert!(h.state().state.call.is_none());
        assert_eq!(records(&h), [(false, 0)]);
        assert!(shown(&h, "Исходящий звонок, 00:00"), "the record row");
        send(&mut h, status(CallStatus::Hangup));
        assert_eq!(records(&h).len(), 1, "the core's hang-up that follows changes nothing");
    }

    #[test]
    fn incoming_call_answered_muted_and_hung_up() {
        let mut h = chat(true);
        send(&mut h, Event::IncomingCall { pubkey: ANYA.to_vec() });
        assert!(shown(&h, "Входящий звонок") && shown(&h, "Сбросить"));
        h.get_by_label("Ответить").click();
        h.run();
        assert_eq!(h.state().state.call.as_ref().unwrap().phase, Phase::Incoming, "waits for the core's InCall");
        send(&mut h, status(CallStatus::InCall));
        assert!(shown(&h, "00:00") && shown(&h, "Завершить"));
        send(&mut h, Event::CallAudio(AudioError::Microphone("no input device".into())));
        assert!(shown(&h, "Микрофон не найден"), "the call goes on and says why there is no sound");
        h.get_by_label("Выкл. звук").click();
        h.run();
        assert!(h.state().state.call.as_ref().unwrap().muted && shown(&h, "Вкл. звук"));
        // A minute in.
        let call = h.state_mut().state.call.as_mut().unwrap();
        call.phase = Phase::Active(Instant::now() - Duration::from_secs(65));
        call.answered_ms = Some(now_ms() - 65_000);
        h.run();
        assert!(shown(&h, "01:05"), "the timer");
        h.get_by_label("Завершить").click();
        h.run();
        assert!(h.state().state.call.is_none());
        let rec = records(&h);
        assert!(matches!(rec[..], [(true, ms)] if (65_000..70_000).contains(&ms)), "{rec:?}");
        assert!(shown(&h, "Входящий звонок, 01:05"), "the record row");
    }

    #[test]
    fn missed_declined_and_busy() {
        let mut h = chat(true);
        send(&mut h, Event::IncomingCall { pubkey: ANYA.to_vec() });
        send(&mut h, status(CallStatus::Hangup));
        assert!(h.state().state.call.is_none(), "the caller gave up");
        assert!(shown(&h, "Пропущенный звонок"));

        send(&mut h, Event::IncomingCall { pubkey: ANYA.to_vec() });
        send(&mut h, Event::IncomingCall { pubkey: vec![8; 32] });
        assert_eq!(h.state().state.call.as_ref().unwrap().pubkey, ANYA, "busy: the second caller is not taken");
        send(&mut h, Event::CallStatusChanged { status: CallStatus::Hangup, pubkey: Some(vec![8; 32]) });
        assert!(h.state().state.call.is_some(), "the other caller's hang-up");
        h.get_by_label("Сбросить").click();
        h.run();
        assert!(h.state().state.call.is_none());
        assert_eq!(records(&h), [(true, 0), (true, 0)]);
    }

    #[test]
    fn unreached_contact_is_given_up_on() {
        let mut h = chat(true);
        h.get_by_label("Позвонить").click();
        h.run();
        h.state_mut().state.call.as_mut().unwrap().started_ms -= DIAL_TIMEOUT_MS + 1000;
        h.run();
        assert!(h.state().state.call.is_none());
        assert_eq!(h.state().state.status.as_ref().map(|s| s.0.as_str()), Some("Не удалось дозвониться: контакт недоступен"));
        h.get_by_label("Позвонить").click();
        h.run();
        send(&mut h, status(CallStatus::Calling));
        h.state_mut().state.call.as_mut().unwrap().started_ms -= DIAL_TIMEOUT_MS + 1000;
        h.run();
        assert!(h.state().state.call.is_some(), "a ringing call is the core's to time out");
    }

    #[test]
    fn answer_needs_ffmpeg() {
        let mut h = chat(false);
        send(&mut h, Event::IncomingCall { pubkey: ANYA.to_vec() });
        assert!(h.get_by_label("Установите ffmpeg, чтобы звонить").accesskit_node().is_disabled());
        assert!(!h.get_by_label("Сбросить").accesskit_node().is_disabled(), "declining needs no audio");
    }

    #[test]
    fn exit_hangs_up_only_a_call_acted_on() {
        let mut h = chat(true);
        send(&mut h, Event::IncomingCall { pubkey: ANYA.to_vec() });
        on_exit(h.state_mut());
        assert!(h.state().state.call.is_some(), "still ringing: nobody acted on it");
        send(&mut h, status(CallStatus::InCall));
        on_exit(h.state_mut());
        assert!(h.state().state.call.is_none());
    }

    const MEDIATOR: [u8; 32] = [5; 32];

    fn invite() -> Invite {
        Invite { mediator: MEDIATOR.to_vec(), session_id: (0..16).collect(), key: (0..32).map(|i: u8| i.wrapping_mul(7)).collect() }
    }

    #[test]
    fn invite_links_like_android() {
        let link = invite_link(&invite());
        let med = hex::encode(MEDIATOR);
        assert_eq!(link, format!("mimir://gc/{med}/AAAQEAYEAUDAOCAJBIFQYDIOB4#AAcOFRwjKjE4P0ZNVFtiaXB3foWMk5qhqK-2vcTL0tk"));
        assert_eq!(parse_invite(&format!("  {link}\n")), Some(invite()));
        assert_eq!(parse_invite(&link.replace("AAAQEAYEAUDAOCAJBIFQYDIOB4", "aaaqeayeaudaocajbifqydiob4")), Some(invite()), "any case");
        for bad in [
            format!("mimir://gc/{med}/AAAQEAYEAUDAOCAJBIFQYDIOB4"),
            format!("mimir://gc/{med}/AAAQEAYEAUDAOCAJBIFQYDIO#AAcOFRwjKjE4P0ZNVFtiaXB3foWMk5qhqK-2vcTL0tk"),
            format!("mimir://gc/{}/AAAQEAYEAUDAOCAJBIFQYDIOB4#AAcOFRwjKjE4P0ZNVFtiaXB3foWMk5qhqK-2vcTL0tk", &med[2..]),
            format!("mimir://gc/{med}/AAAQEAYEAUDAOCAJBIFQYDIOB1#AAcOFRwjKjE4P0ZNVFtiaXB3foWMk5qhqK-2vcTL0tk"),
            format!("mimir://gc/{med}/AAAQEAYEAUDAOCAJBIFQYDIOB4#AAcO"),
            format!("mimir://g/{med}/AAAQEAYEAUDAOCAJBIFQYDIOB4#AAcOFRwjKjE4P0ZNVFtiaXB3foWMk5qhqK-2vcTL0tk"),
        ] {
            assert_eq!(parse_invite(&bad), None, "{bad}");
        }
    }

    #[test]
    fn phone_icons_are_the_mdi_svgs() {
        let h = chat(true);
        assert!(crate::test_support::textured(&h).iter().any(|s| *s == Vec2::splat(20.)), "the header's handset is a picture");
    }

    /// Group 7 «Семья» open on a started offline core (sends are stored, then find no mediator); the 1:1 lane's jobs
    /// (create/join/leave) report nowhere, so the tests send their results themselves.
    fn group(mediator: bool) -> Harness<'static, MimirDesktop> {
        let call_mediator = if mediator { hex::encode(MEDIATOR) } else { String::new() };
        let mut h = test_harness(Settings { intro_done: true, call_mediator, ..Default::default() });
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        let core = app.core.clone();
        crate::test_support::start_offline(&core, app.events.clone(), &ctx);
        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        app.worker = crate::core::worker::Worker::spawn(core.clone(), std::sync::mpsc::channel().0, ctx.clone());
        app.call_audio = CallAudio::new(core.clone(), Some(PathBuf::from("ffmpeg-for-tests")), std::sync::mpsc::channel().0, egui::Context::default());
        app.state.core_started = true;
        app.state.chats = build_chat_list(&core, Lang::Ru);
        app.state.selected = Some(ChatKey::Group(7));
        h.run();
        h
    }

    #[test]
    fn group_call_needs_the_call_mediator() {
        let mut h = group(false);
        h.get_by_label("Новый групповой звонок").click();
        h.run();
        assert!(h.state().state.group_call.is_none());
        let status = h.state().state.status.as_ref().map(|s| s.0.clone());
        assert_eq!(status.as_deref(), Some("Сначала задайте медиатор звонков в расширенных настройках."));
    }

    #[test]
    fn group_call_started_posts_its_invite() {
        let mut h = group(true);
        h.get_by_label("Новый групповой звонок").click();
        h.run();
        assert!(shown(&h, "Групповой звонок") && shown(&h, "Подключение…"));
        assert!(h.get_by_label("Новый групповой звонок").accesskit_node().is_disabled(), "one call at a time");
        let link = invite_link(&invite());
        let t = token(&h);
        send(&mut h, Event::WorkDone(WorkResult::GroupCallUp { token: t, link: link.clone() }));
        assert!(h.state().state.group_call.as_ref().unwrap().since.is_some() && shown(&h, "00:00"));
        let core = h.state().core.clone();
        crate::test_support::run_until(&mut h, "the invite in the group", |_| {
            core.get_group_messages(7, 0, 10).iter().any(|m| m.data.as_deref() == Some(link.as_bytes()))
        });
        h.get_by_label("Выйти").click();
        h.run();
        assert!(h.state().state.group_call.is_none());
        assert!(!h.get_by_label("Новый групповой звонок").accesskit_node().is_disabled());
    }

    #[test]
    fn group_call_audio_error_is_shown() {
        let mut h = group(true);
        h.get_by_label("Новый групповой звонок").click();
        h.run();
        let t = token(&h);
        send(&mut h, Event::WorkDone(WorkResult::GroupCallUp { token: t, link: invite_link(&invite()) }));
        send(&mut h, Event::CallAudio(AudioError::Microphone("unplugged".into())));
        assert!(shown(&h, "Микрофон не найден"), "the group call says why nobody hears us");
    }

    #[test]
    fn group_call_failure_is_shown() {
        let mut h = group(true);
        h.get_by_label("Новый групповой звонок").click();
        h.run();
        let t = token(&h);
        send(&mut h, Event::WorkDone(WorkResult::GroupCallFailed { token: t, err: "no route".into() }));
        assert!(h.state().state.group_call.is_none());
        assert_eq!(h.state().state.status.as_ref().map(|s| s.0.as_str()), Some("Ошибка звонка: no route"));
    }

    #[test]
    fn group_call_link_in_a_bubble_asks_then_joins() {
        let link = invite_link(&invite());
        let text = link.clone();
        let mut h = chat_with(true, move |core, c| {
            core.add_local_message(c, 1, 0, true, false, now_ms(), 0, 0, text.into_bytes()).unwrap();
        });
        // The chat list's preview may carry the same text.
        let bubble = |h: &Harness<'static, MimirDesktop>| h.query_all_by_label(&link).find(|n| n.rect().left() > 250.).unwrap().click();
        bubble(&h);
        h.run();
        assert!(shown(&h, "Войти в групповой звонок?"));
        h.get_by_label("Отмена").click();
        h.run();
        assert!(h.state().state.join_group_call.is_none() && h.state().state.group_call.is_none());
        bubble(&h);
        h.run();
        h.get_by_label("Войти").click();
        h.run();
        assert!(h.state().state.join_group_call.is_none());
        assert!(h.state().state.group_call.as_ref().is_some_and(|g| g.since.is_none()), "joining");
        assert!(h.get_by_label("Позвонить").accesskit_node().is_disabled(), "no 1:1 call meanwhile");
        send(&mut h, Event::IncomingCall { pubkey: ANYA.to_vec() });
        assert!(h.state().state.call.is_none(), "busy");
        let t = token(&h);
        send(&mut h, Event::WorkDone(WorkResult::GroupCallUp { token: t, link }));
        assert!(h.state().state.group_call.as_ref().is_some_and(|g| g.since.is_some()));
    }

    #[test]
    fn group_call_roster_and_its_end() {
        let mut h = chat(true);
        let link = invite_link(&invite());
        crate::platform::handle(h.state_mut(), &egui::Context::default(), &crate::platform::Ext::Link(link.clone()));
        h.run();
        h.get_by_label("Войти").click();
        h.run();
        let t = token(&h);
        send(&mut h, Event::WorkDone(WorkResult::GroupCallUp { token: t, link: link.clone() }));
        let me = h.state().core.public_key();
        let part = |pubkey: Vec<u8>, name: &str| mimir::GroupCallParticipant { pubkey, display_name: name.into(), audio_config: vec![], joined_at: 0 };
        let sid = vec![0; 16];
        send(&mut h, Event::GroupCallParticipants { session_id: sid.clone(), participants: vec![part(me.clone(), "Я"), part(vec![8; 32], "Оля")] });
        assert!(shown(&h, "Я (вы)") && shown(&h, "Оля"));
        send(&mut h, Event::GroupCallParticipantEvent { session_id: sid.clone(), event: 1, pubkey: vec![0xab; 32], display_name: String::new() });
        assert!(shown(&h, "abababab"), "no name: the key's start");
        send(&mut h, Event::GroupCallParticipantEvent { session_id: sid.clone(), event: 2, pubkey: vec![8; 32], display_name: String::new() });
        assert!(!shown(&h, "Оля"));
        h.get_by_label("Выкл. звук").click();
        h.run();
        assert!(h.state().state.group_call.as_ref().unwrap().muted && shown(&h, "Вкл. звук"));
        send(&mut h, Event::GroupCallError { code: 3, message: "session full".into() });
        assert!(h.state().state.group_call.is_none());
        assert_eq!(h.state().state.status.as_ref().map(|s| s.0.as_str()), Some("session full"));

        // Busy, a bad link, and the mediator dropping us.
        crate::platform::handle(h.state_mut(), &egui::Context::default(), &crate::platform::Ext::Link("mimir://gc/zz".into()));
        assert_eq!(h.state().state.status.as_ref().map(|s| s.0.as_str()), Some("Неверная ссылка на приглашение в групповой звонок."));
        crate::platform::handle(h.state_mut(), &egui::Context::default(), &crate::platform::Ext::Link(link.clone()));
        h.run();
        h.get_by_label("Войти").click();
        h.run();
        crate::platform::handle(h.state_mut(), &egui::Context::default(), &crate::platform::Ext::Link(link));
        assert_eq!(h.state().state.status.as_ref().map(|s| s.0.as_str()), Some("Звонок уже в процессе!"));
        send(&mut h, Event::GroupCallDisconnected);
        assert!(h.state().state.group_call.is_none());
    }
    /// The current attempt's token (0: none).
    fn token(h: &Harness<'static, MimirDesktop>) -> u64 {
        h.state().state.group_call.as_ref().map_or(0, |g| g.token)
    }

    #[test]
    fn stale_group_call_results_are_dropped() {
        let mut h = chat(true);
        // The core is not started: each create/join fails at once, into `jobs`; leaves report Done.
        let (tx, jobs) = std::sync::mpsc::channel();
        let app = h.state_mut();
        app.worker = crate::core::worker::Worker::spawn(app.core.clone(), tx, egui::Context::default());
        app.settings.call_mediator = hex::encode(MEDIATOR);
        start_group(app, 7);
        let first = token(&h);
        h.run();
        h.get_by_label("Выйти").click();
        h.run();
        let link = invite_link(&invite());
        crate::platform::handle(h.state_mut(), &egui::Context::default(), &crate::platform::Ext::Link(link.clone()));
        h.run();
        h.get_by_label("Войти").click();
        h.run();
        let second = token(&h);
        assert_ne!(first, second);
        send(&mut h, Event::WorkDone(WorkResult::GroupCallFailed { token: first, err: "timeout".into() }));
        assert_eq!(token(&h), second, "the first attempt's failure leaves the new call alone");
        assert_eq!(h.state().state.status, None);
        send(&mut h, Event::WorkDone(WorkResult::GroupCallUp { token: first, link: link.clone() }));
        assert!(h.state().state.group_call.as_ref().is_some_and(|g| g.since.is_none()), "nor does its late success");
        send(&mut h, Event::WorkDone(WorkResult::GroupCallUp { token: second, link: link.clone() }));
        assert!(h.state().state.group_call.as_ref().is_some_and(|g| g.since.is_some()));
        h.get_by_label("Выйти").click();
        h.run();
        // With no call left, a stale success is left at once.
        send(&mut h, Event::WorkDone(WorkResult::GroupCallUp { token: second, link }));
        assert!(h.state().state.group_call.is_none());
        let results: Vec<_> = (0..5).map(|_| jobs.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
        let leaves = results.iter().filter(|r| matches!(r, Event::WorkDone(WorkResult::Done))).count();
        let failed = results.iter().filter(|r| matches!(r, Event::WorkDone(WorkResult::GroupCallFailed { .. }))).count();
        assert_eq!((leaves, failed), (3, 2), "two «Выйти» and the stale success; {results:?}");
        assert!(jobs.recv_timeout(Duration::from_millis(300)).is_err(), "no other leave (it would end a newer call)");
    }
}
