# MPD Bot

> **Planning status:** implementation is paused for architecture review. The current source is an unverified exploratory prototype; its browser UI is superseded by the confirmed native desktop window + tray requirement. Start with [the proposed architecture](ARCHITECTURE.md) and [the implementation plan](PLAN.md). The notes below describe the prototype, not an approved or verified release.

A Rust companion for Twitch chat. One local process receives chat over **Twitch EventSub WebSockets**, calls your AI provider, and posts replies with **Twitch's Send Chat Message API**. Configure it in your browser, then close the tab while you stream.

This is a new implementation inspired by [kc2-io/streamerbot-ai](https://github.com/kc2-io/streamerbot-ai), originally based on Mustached_Maniac's [ChatGPT Bot Integration](https://extensions.streamer.bot/t/chatgpt-bot-integration/865). The original C# source was inspected in `legacy-reference/`; it is not part of this application's build.

## Included

- Direct Twitch connection using EventSub and Helix; Streamer.bot is not required.
- OpenAI **Responses API**, Anthropic Messages, OpenRouter, and configurable OpenAI-compatible chat completions endpoints.
- Editable personality, system prompt, exact model ID, API keys, and Twitch settings.
- Private test replies, generated using saved settings; previews never post to chat.
- Replies to `!ai your question` or a message starting with `@bot_login`.
- Per-viewer conversation memory, exclusions, a cooldown, and bounded input/output sizes.
- Windows Credential Manager, macOS Keychain, and Linux Secret Service credential storage. Session-only and environment-variable credentials also work.
- Automatic EventSub reconnection, subscription transfer on Twitch-directed reconnects, resubscription after network failures, bounded duplicate detection, hourly token validation, and delivery-result checking.

## Build and run

Install a current stable [Rust toolchain](https://rust-lang.org/tools/install/).

```sh
cargo test --locked
cargo build --release --locked
```

On Windows use the MSVC Rust toolchain with Visual Studio C++ Build Tools, or GNU Rust with MinGW-w64 on PATH. On macOS install Xcode Command Line Tools. On Linux install a C compiler, make, pkg-config, and development headers; D-Bus is vendored during the build. Saving keys on Linux requires a running, unlocked Secret Service implementation such as GNOME Keyring or KWallet with Secret Service support. Session-only keys and environment variables work without a desktop keychain.

Run `target/release/mpd-bot` (Windows: `target\release\mpd-bot.exe`). Open the full private configuration link printed in the terminal. The random URL fragment authenticates this browser tab to the local app; it changes each time the bot starts.

```sh
mpd-bot --port 9847
mpd-bot --data-dir /path/to/settings
```

Default settings are stored in the operating system's per-user configuration directory, printed at startup. The listener binds only to `127.0.0.1`. Keep the process running; Ctrl+C stops it. A browser, Node.js, and Rust are not required at runtime except that a browser is needed while configuring the app.

## Configure AI

1. Select a provider and paste the **exact model ID** available to your account. Model IDs are not rewritten or restricted to a built-in list.
2. Save the provider's API key. Each provider has its own credential entry.
3. Set the bot name, personality, and system prompt. Save changes.
4. Use **Generate test reply** to check the result. This makes a billable request with your provider's credentials.

OpenAI uses `/v1/responses` with `store: false`. Anthropic uses `/v1/messages`. OpenRouter uses `/api/v1/chat/completions`. Compatible mode takes a **full chat completions URL**, including its path; it permits HTTPS and HTTP on loopback for local services. Only the dedicated compatible-provider key is sent to a custom endpoint. A key is optional for that provider. No unsupported temperature parameter is forced onto reasoning models. The output token budget includes reasoning tokens where applicable; increase it if a reasoning model returns no visible text.

ChatGPT subscriptions and OpenAI API billing are separate. Use a provider API key, not a ChatGPT session cookie.

## Configure Twitch

1. Obtain a **user access token** for the Twitch account that will speak as the bot. It needs **`user:read:chat`** and **`user:write:chat`** scopes. Follow [Twitch's OAuth documentation](https://dev.twitch.tv/docs/authentication/getting-tokens-oauth/) or use [Twitch CLI token generation](https://dev.twitch.tv/docs/cli/token-command/). An app access token or old IRC-only scopes will not work.
2. Enter that account's lowercase login under **Bot account login** and your channel's login under **Channel login**, without `@` or `#`.
3. Save the Twitch token, enable **Connect**, and save changes.
4. Check **At a glance** for `EventSub connected`, then type `!ai hello` in your channel.

The app gets the client ID and bot user ID from Twitch's token validation endpoint, then resolves the channel login to its broadcaster ID. You do not need to type numeric IDs or configure an inbound webhook. The connected bot account must be permitted to chat in your channel. Twitch moderation and channel restrictions still apply.

**Token lifecycle:** this version accepts an existing access token. It validates at connection and hourly, stops on authorization failure, and asks for a replacement token when expired or revoked. It does not yet include a browser OAuth sign-in flow or automatic token refresh. Saving settings retries a failed connection.

**Shared Chat:** relayed messages originating in other channels are ignored. Twitch user-token replies are shared across an active Shared Chat session according to Twitch's rules; user access tokens cannot set `for_source_only`.

## Resource design

- A single-thread Tokio async runtime handles network I/O. Short-lived blocking workers are used for OS keychain and atomic settings writes.
- One reusable HTTP client and one EventSub socket in normal operation. A second socket exists briefly during Twitch's reconnect handoff.
- **One AI request at a time**, with no request backlog. New requests are skipped while busy or in cooldown. Ordinary chat never calls an AI provider.
- Default memory: **64 viewers × 6 turns**. Limits are configurable up to 128 viewers and 20 turns. Each input is limited to 4,000 bytes. History is evicted by least recent use and cleared on settings changes or exit.
- 45-second HTTP deadline, 5-second connection timeout, 256 KiB provider-response and WebSocket-message limits, and 32 KiB local request bodies.
- Provider output is flattened to one line and capped to the configured character limit. Replies plus a username fit Twitch's 500-character limit.
- The UI contains embedded HTML/CSS/JavaScript, with no frontend framework, Node server, remote fonts, trackers, or runtime asset downloads. Status requests run every five seconds only while the tab is visible.
- Release builds optimize for size. Memory and CPU claims should be based on measurements of the actual release binary; Rust by itself is not a resource guarantee.

## Credentials and local access

Settings JSON contains no API keys. Environment variables are read at startup and take precedence over OS-stored credentials:

| Credential | Environment variable |
| --- | --- |
| OpenAI | `OPENAI_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Compatible endpoint | `COMPATIBLE_API_KEY` |
| Twitch | `TWITCH_ACCESS_TOKEN` |

Keys are write-only in the UI/API and are not logged. The OS credential service name is `io.kc2.mpd-bot`. All instances under the same OS account share these credential entries, even with different settings directories. A session-only override does not erase an older saved key. **Remove** deletes the saved credential and clears the current in-memory value; an environment variable will take effect again at restart.

The local API requires a fresh random bearer token and validates Host/Origin. It sends no CORS allow headers. Treat the full configuration link as a password and keep it out of stream captures. The app does not persist chat transcripts, but prompts and relevant conversation history are sent to your chosen provider. `store: false` is not a claim about all provider logging or retention.

## Layout

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Startup, local listener, task lifecycle |
| `src/config.rs` | Typed configuration, validation, atomic persistence |
| `src/secrets.rs` | OS credentials and session/environment sources |
| `src/provider.rs` | Provider HTTP formats and response parsing |
| `src/engine.rs` | Request admission, memory, output cleanup |
| `src/twitch.rs` | EventSub lifecycle and Helix delivery |
| `src/server.rs` | Authenticated configuration and preview API |
| `ui/` | Embedded configuration interface |

Transport handling is separate from provider calls and the conversation engine. A future Streamer.bot adapter can call the same engine. No Streamer.bot bridge is included in this first version. Legacy voice transcription, shoutouts, random chat replies, Fortnite events, and other platform connections have not been ported.

## Verification

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
```

Tests use synthetic provider responses, local mock servers, and isolated configuration directories. They do not call paid AI providers or post to Twitch. Live provider/Twitch testing requires credentials and a channel; successful offline tests do not establish a live integration passed.

## Protocol references

- [Twitch recommends EventSub and API calls for new chatbots](https://dev.twitch.tv/docs/chat/irc)
- [EventSub WebSocket lifecycle](https://dev.twitch.tv/docs/eventsub/handling-websocket-events/)
- [Channel Chat Message subscription](https://dev.twitch.tv/docs/eventsub/eventsub-subscription-types/#channelchatmessage)
- [Send Chat Message](https://dev.twitch.tv/docs/api/reference/#send-chat-message)
- [OpenAI Responses API](https://developers.openai.com/api/reference/cli/resources/responses/methods/create)
- [Anthropic Messages API](https://platform.claude.com/docs/en/api/messages/create)
- [OpenRouter API](https://openrouter.ai/docs/quickstart)
