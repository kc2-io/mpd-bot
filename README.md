# MPD Bot

**A twitch bot with real personality**

## About

MPD Bot is a desktop companion that gives your Twitch chat an AI-powered personality of its own. Instead of a generic bot replying to commands, MPD Bot reads your live chat and jumps in the way a real chatter would — with a personality, tone and voice that you define.

Why streamers want this:

- **A chat presence that feels alive.** Set a personality and prompt once in the native **Control Room**, and the bot replies in character — occasionally on its own, on command, or when mentioned.
- **Runs alongside your stream, out of the way.** It lives in the Windows system tray while you stream; no browser tab, no extra OBS scene, no console window.
- **You choose the brain.** Bring your own API key for OpenAI, Anthropic, OpenRouter, or any OpenAI-compatible endpoint, and switch between named profiles (e.g. a fast/cheap one and a creative one) without losing your other settings.
- **Safe by default.** Replies respect cooldowns, rate limits and pause controls, and nothing gets sent to Twitch without your configuration allowing it.

The desktop milestone is implemented; automated tests and a Windows release smoke check have passed — see [VALIDATION.md](VALIDATION.md) for exact evidence and remaining checks. **The public Twitch Client ID is configured; live OAuth testing remains pending.**

Inspired by [kc2-io/streamerbot-ai](https://github.com/kc2-io/streamerbot-ai), originally based on Mustached_Maniac's [ChatGPT Bot Integration](https://extensions.streamer.bot/t/chatgpt-bot-integration/865). The legacy C# extension is not part of this application's build.

## Prerequisites

To run MPD Bot you need:

- **An AI provider account and API key** — OpenAI, Anthropic, OpenRouter, or another OpenAI-compatible chat completions endpoint. You supply your own key and pay your own provider's usage.
- **A Twitch account** for the bot to speak as, plus permission to post in the destination channel.
- **A Windows PC.** Windows is the only platform currently verified and supported end-to-end.

**Multi-platform support (macOS, Linux) is coming later.** The source retains macOS and Linux backends, but their runtime behavior, tray integration and resource targets are not yet verified — see [Build and run](#build-and-run) for details on what's present today.

## Technical details

- **Language:** Rust (2024 edition), compiled to a single native executable — no Node.js, webview, Electron or local AI model runtime required.
- **UI:** [Slint](https://slint.dev/) with the Winit backend and software renderer, including native system tray support.
- **Async runtime:** Tokio (single-thread runtime dedicated to network work; the UI/tray own the main thread).
- **HTTP:** reqwest (rustls-tls) for provider and Twitch API calls.
- **WebSockets:** tokio-tungstenite (rustls-tls, webpki-roots) for Twitch EventSub.
- **Serialization:** serde / serde_json for config, credentials and provider payloads.
- **Other libraries:** `directories` (per-OS config paths), `tempfile` and `fs2` (atomic, locked credential/config writes), `webbrowser` (system browser for Twitch OAuth consent), `rfd` (native file dialogs, e.g. log export), `fastrand`, `png`, `futures-util`.

**External connections MPD Bot makes:**

| System | Purpose |
| --- | --- |
| Twitch EventSub (WebSocket) | Receives live chat messages |
| Twitch Send Chat Message API | Posts the bot's replies |
| Twitch device OAuth | Authenticates the bot's Twitch identity, with automatic token refresh |
| AI provider (OpenAI Responses, Anthropic Messages, OpenRouter, or a configured OpenAI-compatible endpoint) | Generates reply text from chat context and your configured personality/prompt |

No other network services are contacted. See [ARCHITECTURE.md](ARCHITECTURE.md) for component responsibilities and [PLAN.md](PLAN.md) for delivered work and remaining acceptance criteria.

## Included

- Native Slint settings window with the supplied MPD logo and cyan/slate theme, Windows tray Open/Pause/Resume/Quit, and no browser configuration server.
- OpenAI Responses, Anthropic Messages, OpenRouter, and explicitly configured OpenAI-compatible chat completions endpoints.
- Named API profiles with independent provider, model, endpoint and key; multiple profiles can use the same provider. Personality, prompts, limits, account exclusions and private test replies remain shared bot settings.
- Twitch device OAuth from the UI, automatic token refresh, app-owned credential files and reconnect recovery.
- Random replies to eligible chat messages (10% default, configurable from 0–100%). Optional chat command (off by default) and leading `@bot_login` mentions (on by default), each with its own toggle under **Twitch connection**.
- Saved chatter profiles with nicknames, descriptions, Sarcastic/Praise/Hero/Regular styles, a persisted previously seen list and a Never respond rule.
- Bounded conversation memory committed only after Twitch confirms delivery.
- Session Logs with severity/subsystem filters, search, follow/pause, clear, text selection/copy, and explicit export.

## Releases

Pushing a version tag such as `v0.2.0` builds and publishes a Windows x64 ZIP on GitHub Releases. The tag version is embedded in the app and displayed in About. See [RELEASING.md](RELEASING.md) for the release steps, prereleases, checksums and retry behavior.

## Build and run

On Windows, install stable Rust for **`x86_64-pc-windows-msvc`** and Visual Studio Build Tools with the Desktop development with C++ workload and a Windows SDK. From a Developer PowerShell with Rust on PATH:

```powershell
cargo test --locked
cargo build --release --locked
.\target\release\mpd-bot.exe
```

The release executable opens a desktop window without a console. Slint uses the Winit backend and software renderer. No Node.js, webview or local AI model is required. Twitch consent opens your system browser.

```powershell
.\target\release\mpd-bot.exe --data-dir D:\MyBotSettings
.\target\release\mpd-bot.exe --demo
```

`--demo` starts an isolated settings UI without loading saved credentials or connecting Twitch. `--port` is retired and produces an explanatory error. A per-user instance lock prevents two real bot instances even with different settings directories. A duplicate launch tells you to open the running bot from its tray icon.

Minimize or Close hides the window when the tray is available. Hidden settings drafts are retained and the bot continues working. Pause stops new AI work and cancels unsent replies; Quit stops the process. Pause is a session override; use Save changes to persist the currently displayed active/paused setting for the next launch. If the tray fails, the application keeps a usable window. Ordinary settings saves use native controls; browser password-manager form detection no longer applies. API keys and Twitch authorization are saved by the app; there is no storage checkbox or OS credential-vault integration.

The source retains Windows, macOS and Linux backends. macOS builds need Xcode command-line tools; Linux needs the native desktop/build dependencies listed by the CI workflow. Credential files do not require Secret Service or a desktop keychain. Linux tray availability depends on the desktop's StatusNotifier support. These platforms require runtime acceptance testing before claiming support equivalent to Windows.

## Configure AI

1. Open **AI profiles**, choose **New profile**, and enter a unique name such as "OpenAI fast", "OpenAI creative" or "Claude main". Select the provider and enter its exact model ID, optional compatible endpoint and API key.
2. Click **Save & use profile**. This saves all API details together and makes that profile active. Select another saved profile from the dropdown to switch; the active selection survives restart. Up to 16 profiles are supported within a 64 KiB credential file.
3. Edit the name to rename a profile. A blank key field keeps the saved key when the provider and endpoint are unchanged. Changing provider or endpoint clears the previous key unless a replacement is entered. **Discard edits** restores the saved profile; **Remove saved key** removes only its key. **Delete profile** removes the profile and key after confirmation, selecting another profile if needed. At least one profile must remain.
4. Set the bot name, personality and prompt; save changes. These settings are shared across API profiles. Save or discard profile drafts before switching, and save other settings before activating a profile.
5. Use **Generate test reply**. It uses the active saved profile and API credits, but never posts to Twitch or enters live conversation memory.

| Provider | Request format |
| --- | --- |
| OpenAI | `https://api.openai.com/v1/responses`, with `store: false` |
| Anthropic | `https://api.anthropic.com/v1/messages` |
| OpenRouter | `https://openrouter.ai/api/v1/chat/completions` |
| Compatible | Explicit full chat-completions endpoint; HTTPS, or HTTP on loopback |

Model IDs are editable and are not silently rewritten. Compatible mode uses its own optional credential. The app does not force temperature or perform automatic provider failover. Some reasoning models need a larger output-token budget to produce visible text. Provider failures show curated categories, HTTP status and available bounded request IDs; HTTP 404 alone does not prove the model ID is invalid.

## Connect Twitch

### Maintainer prerequisite

MPD Bot's public Twitch Client ID is configured in `.cargo/config.toml` and embedded by normal Cargo builds. To use a different Public Twitch application during development, override `MPD_BOT_TWITCH_CLIENT_ID` while building or at runtime:

```powershell
$env:MPD_BOT_TWITCH_CLIENT_ID = 'YOUR_PUBLIC_TWITCH_CLIENT_ID'
.\target\release\mpd-bot.exe
```

The runtime value takes precedence over a build-time value. A client ID is public; do not supply or embed a client secret. Without a client ID, the UI explains why sign-in is unavailable. End users of a configured release will not need their own developer application.

### Normal setup

1. Select **Connect Twitch** in the desktop UI.
2. Approve the requested access on Twitch using the intended bot account. The app opens Twitch's verification page and provides the authorization code/link in the connection view.
3. Confirm the connected account, choose the destination channel, enable its connection and save settings.
4. Under **Twitch connection**, set the random reply percentage and choose whether to enable a chat command or replies to leading @mentions. Check the connection status before testing in your channel.

The percentage is an independent chance for each eligible message, not an exact quota. At 0%, only enabled commands/mentions can request a reply; at 100%, every eligible message is considered. Self messages, ignored accounts, duplicate events, relayed messages from other channels and other bots' `!commands` are skipped. Disabled direct mentions are also skipped. Enabled commands and mentions bypass random selection, but all live replies still obey pause, cooldown (at least three seconds), one-request-at-a-time admission and Twitch rate limits. Busy messages are skipped without a backlog.

The required scopes are `user:read:chat` and `user:write:chat`. Twitch supplies the bot identity; users do not paste access or refresh tokens. The bot must be allowed to speak in the destination channel.

The app stores the access/refresh pair together in its credentials directory, validates at startup and hourly, and renews credentials automatically. Revocation or an unusable refresh token requires **Reconnect Twitch**; this is a renewable login, not a permanent token. File-storage failures are shown in the UI rather than silently reporting a successful save. Disconnect removes the local authorization file and attempts remote revocation.

Relayed Shared Chat messages from other source channels are ignored. Replies made with a user token follow Twitch's Shared Chat distribution rules; user tokens cannot request `for_source_only`.

## Configure chatters

Open **Chatters** and choose **Add chatter by username** or **Choose previously seen**. Enter an optional nickname and short description, select any combination of **Sarcastic**, **Praise**, **Hero** and **Regular**, then click **Save chatter**. These guide wording while the normal reply probability and limits still apply. Profiles apply across channels within the selected local configuration.

**Never respond** overrides random replies, commands and mentions after saving. Existing ignored accounts remain excluded independently. A new block applies immediately in memory even if its disk save fails; the UI reports that failure so you can retry. A reply already dispatched to Twitch may still arrive. Removing a block requires a successful save.

Only the current chatter's saved details are included in their AI request. Nicknames do not replace real Twitch mentions. Typed usernames work offline and bind to the account's stable Twitch ID when observed, so a later username change does not transfer that profile to another account.

Previously seen records contain account ID, login, last-seen time and channel ID, without chat text. They are collected while the bot is connected and active, retained for up to 90 days and capped at 2,000 accounts/1 MiB. Clear or forget seen history leaves curated profiles intact. Observations are saved roughly every 30 seconds; recent observations can be lost on an abrupt exit.

Curated profiles are capped at 500/2 MiB, with 64-character nicknames and 500-character descriptions. They do not expire automatically. Both versioned JSON files live under `<config-directory>/chatters`, using restricted access and atomic replacement, with no encryption. **Memory & limits** includes a separate approximate chatter-data estimate, not a measurement of process RAM.

## Existing settings and credentials

The default settings directory is unchanged. On Windows it is normally `%APPDATA%\kc2\mpd-bot\config`. An old, unversioned POC `config.json` loads as schema version 1 and gains the version field when saved. Invalid or unsupported files produce an error rather than being overwritten. Stop the earlier browser POC before launching the desktop build.

API keys and Twitch authorization are kept in app-owned, versioned JSON files under `<config-directory>/credentials`:

| File | Contents |
| --- | --- |
| `ai-profiles.json` | Named profiles, independent keys/models/endpoints, and active profile ID |
| `provider-{id}.json` | Legacy provider keys, imported only when no profile store exists |
| `twitch-{clientid}.json` | Twitch access/refresh pair, validated identity, scopes and expiry |

The JSON is **unencrypted** and separate from `config.json` and logs. Files are replaced atomically with access restrictions applied before secret bytes are written. Unix uses mode `0600` for files and `0700` for the directory. Windows replaces the file/directory access list with permissions for the current account and SYSTEM through a hidden PowerShell/.NET helper; inherited and unrelated explicit access entries are removed.

This build does not use Windows Credential Manager, macOS Keychain or Linux Secret Service, and does not automatically import their old entries. When upgrading from that version, enter each API key and use **Connect Twitch** once; subsequent restarts restore the new files. The old OS entries are left untouched.

On the first upgrade to named profiles, the current provider/model becomes the active profile, and each additional configured provider gets a profile with its saved key (enter its model before use). Existing provider key files remain untouched. The profile store is authoritative afterward; removed keys are not silently reimported. An invalid profile store is reported without being overwritten.

During this one-time import, provider environment variables take precedence over legacy provider files:

| Provider | Environment variable |
| --- | --- |
| OpenAI | `OPENAI_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Compatible endpoint | `COMPATIBLE_API_KEY` |

Profile keys remain masked/write-only in the UI and never enter ordinary config.json or log exports. Keys and metadata are atomically saved in one protected profile store; a failed save preserves the previous applied profile. Provider environment variables do not override existing named profiles. Switching profiles cancels unsent AI work and clears conversation memory. `--data-dir` selects a separate settings and credentials directory; the global per-user lock still permits only one real bot instance at a time.

OAuth credentials are namespaced by public Client ID in the file name. Rotated refresh credentials replace the same record. An old access-only Twitch token cannot be converted into refresh credentials: use Connect Twitch.

**Development compatibility only:** `MPD_BOT_LEGACY_TWITCH=1` explicitly permits `TWITCH_ACCESS_TOKEN`, together with the saved bot login. It does not import an old OS-stored token. This mode has no refresh token and is not the normal setup UI. A connected OAuth identity takes precedence. Existing pasted tokens are not used silently by default.

## Logs, privacy and resource bounds

Logs retain curated application events in memory, capped at **1,000 events and 2 MiB**. Events use fixed summaries and approved bounded fields; raw provider bodies, API keys, OAuth credentials/codes/links, prompts and chat text are excluded. Busy log producers drop events instead of blocking and expose a dropped-event count. Export writes only the sanitized session log to the file you choose.

Logs are not automatically persisted. The separate `last-crash.txt` marker contains only a fixed application/version message, without the panic payload. Conversation memory is also session-only. Selected prompts and relevant context are sent to your chosen provider; provider data policies still apply.

The UI and tray own the main thread. A separate single-thread Tokio runtime handles network work, with blocking storage delegated to workers. UI updates are event-driven; the hidden window does not poll status. Commands are bounded, only one AI request/delivery is admitted, and excess chat is skipped rather than queued. Default memory is 64 viewers × 6 remembered viewer messages, each kept with its matching bot reply, with a 2 MiB content budget. **Memory & limits** shows a live estimate for full chat history based on remembered messages, viewers and maximum reply characters. It uses maximum-length UTF-8 text plus approximate collection overhead; it is not total process RAM. App/UI, logs and active requests use additional memory. Set remembered messages to zero to disable conversation memory. The persisted `memory_turns` field retains its existing meaning for compatibility. Provider/WebSocket messages are bounded at 256 KiB. Replies including mentions fit Twitch's 500-character limit.

These are implementation bounds, not measured promises for total process memory or CPU. Full performance and long-stream acceptance remain in [PLAN.md](PLAN.md) and [VALIDATION.md](VALIDATION.md).

## Code and verification

See [ARCHITECTURE.md](ARCHITECTURE.md) for responsibilities and [PLAN.md](PLAN.md) for delivered work and remaining acceptance.

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

Tests use synthetic responses, local mock servers, and isolated files/stores; routine tests do not call paid providers or post chat. Windows release smoke testing is distinct from live OAuth/Twitch validation. Slint attribution is available from About. Streamer.bot integration, voice, shoutouts, additional platforms, auto-start and auto-update remain outside this milestone.

### Protocol references

- [Twitch device authorization](https://dev.twitch.tv/docs/authentication/getting-tokens-oauth/#device-code-grant-flow)
- [Token validation](https://dev.twitch.tv/docs/authentication/validate-tokens/)
- [EventSub WebSocket lifecycle](https://dev.twitch.tv/docs/eventsub/handling-websocket-events/)
- [Send Chat Message](https://dev.twitch.tv/docs/api/reference/#send-chat-message)
- [Slint tray support](https://docs.slint.dev/latest/docs/slint/reference/window/systemtrayicon/)
