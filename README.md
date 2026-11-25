<p align="center">
  <img src="assets/icons/app.png" width="96" alt="">
</p>

<h1 align="center">Mimir Desktop</h1>

<p align="center">
  Desktop client for the <a href="https://github.com/Mimir-IM/MimirForAndroid">Mimir</a> messenger.<br>
  Peer-to-peer over the Yggdrasil network. No accounts, no phone numbers.
</p>

<p align="center">
  <a href="https://github.com/JB-SelfCompany/mimir-desktop/actions/workflows/ci.yml"><img src="https://github.com/JB-SelfCompany/mimir-desktop/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/JB-SelfCompany/mimir-desktop/releases"><img src="https://img.shields.io/github/v/release/JB-SelfCompany/mimir-desktop?include_prereleases&label=release&color=49C29C" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MPL--2.0-49C29C" alt="MPL-2.0"></a>
</p>

<p align="center">
  <a href="#download">Download</a> ·
  <a href="#features">Features</a> ·
  <a href="#building-from-source">Build</a> ·
  <a href="https://github.com/Mimir-IM/MimirForAndroid">Mimir for Android</a>
</p>

<p align="center">
  <img src=".github/assets/chat.png" width="860" alt="A one-to-one chat with a photo, a reply and a voice message">
</p>

Mimir Desktop runs the same core library as Mimir for Android and talks to it on equal terms: the same account, contacts, groups, attachments and voice messages. Import a backup from your phone and the computer becomes a second device of your account; both stay in sync.

The window follows the Android app closely, so nothing has to be relearned. It is written in Rust with [egui](https://github.com/emilk/egui) and ships as a single executable for Windows and Linux.

## Features

**Messaging.** One-to-one chats and Saved Messages, replies, reactions, forwarding, drafts, delivery ticks, unread markers.

**Groups.** Private and public groups through mediator servers, invitations and join links, member roles and bans, several mediators side by side.

**Media.** Photos with a built-in viewer, files of any type, voice messages in the same AAC format Android records, video messages from the phone. Drag and drop or paste an image with Ctrl+V.

**Backup.** Import an Android backup to bring your history over, export one to move back. Settings travel with it.

**Desktop.** Lives in the tray, notifies about new messages, group invites and incoming calls, opens `mimir://` links from the browser, and runs one copy per profile.

**Settings.** The same options as on Android: auto-download limits, who may send contact requests, image resizing, voice quality, font size, Yggdrasil peers, mediators. Light and dark theme, English and Russian.

<table>
  <tr>
    <td><img src=".github/assets/chat-light.png" alt="Light theme"></td>
    <td><img src=".github/assets/settings.png" alt="Settings window"></td>
  </tr>
</table>

## Download

Builds for Windows (zip) and Linux (tar.gz) are on the [releases page](https://github.com/JB-SelfCompany/mimir-desktop/releases). Unpack and run; nothing is installed.

To record voice messages, put [`ffmpeg`](https://ffmpeg.org/download.html) next to the executable or on `PATH`. Everything else works without it.

On Linux the app needs GTK 3 and ALSA, which most desktops already have, and `libayatana-appindicator3-1` for the tray icon.

## Building from source

The core library is a path dependency, so clone it next to this repository:

```sh
git clone https://github.com/JB-SelfCompany/mimir-desktop
git clone https://github.com/Mimir-IM/mimir upstream/mimir
git clone https://github.com/Revertron/ygg_stream upstream/ygg_stream
cd mimir-desktop
cargo build --release
```

The core revisions CI builds against are pinned in [`ci.yml`](.github/workflows/ci.yml). On Debian and Ubuntu install the system libraries first:

```sh
sudo apt install libgtk-3-dev libasound2-dev libxkbcommon-dev libxdo-dev libayatana-appindicator3-dev pkg-config
```

Run the tests with `cargo test`. Set `MIMIR_DATA_DIR` to keep a second profile apart from your main one.

## Related projects

- [Mimir for Android](https://github.com/Mimir-IM/MimirForAndroid), the mobile client
- [mimir](https://github.com/Mimir-IM/mimir), the core library
- [mimir-mediator](https://github.com/Mimir-IM/mimir-mediator), the group chat server
- [Yggdrasil](https://yggdrasil-network.github.io/), the network underneath

## License

[Mozilla Public License 2.0](LICENSE). The bundled Noto fonts are under the [SIL Open Font License](assets/fonts/OFL.txt).
