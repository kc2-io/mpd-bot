# MPD Bot — implementation plan

Status: **planning; no further implementation until the architecture has been reviewed**.

Confirmed direction: core chat bot first; Rust; Windows/macOS/Linux; native settings window and tray; Twitch EventSub + Helix; OpenAI, Anthropic, OpenRouter, and compatible APIs. See [ARCHITECTURE.md](ARCHITECTURE.md) for the proposed design.

Confirmed name: **MPD Bot** (`mpd-bot`). The current POC UI is the accepted visual starting point; adapt it to the native window and defer further visual redesign. This naming/design update does not resume feature implementation or finalize the framework.

## Phase 0 — settle the design

Deliverables:

- Confirm v1 scope and the architecture decision table.
- Select reference operating systems/hardware for resource measurements.
- Decide Twitch OAuth application ownership and development Client ID.
- Record native UI framework as a candidate until the feasibility experiment passes.
- Check whether Slint's distribution terms fit the intended project license; retain egui/eframe as the alternative.

Exit: requirements and unresolved product decisions are explicit. Do not use the existing prototype as an implicit source of requirements.

## Phase 1 — native shell and resource experiment

Build only a small native window, tray icon, lifecycle controls, and sleeping background task.

Start with Slint's native window and built-in tray. Test an egui/eframe alternative if licensing, functionality, or resource measurements rule Slint out. No need to build two complete applications.

Verify:

- Windows, macOS, and the chosen Linux desktop environments.
- Close/reopen, hide-to-tray, Pause, Quit, keyboard navigation, and basic accessibility.
- Missing tray support never strands the user.
- Main-thread/event-loop correctness and event-driven repaint.
- Idle CPU, process memory, GPU memory, package size, and startup time.

Exit: choose UI framework/renderer and record measured tradeoffs. Reconsider the candidate if it cannot meet resource or platform requirements before building the full UI.

## Phase 2 — typed core and settings

- Define configuration, provider profiles, normalized chat input, prepared reply, and delivery-outcome types.
- Implement application lifecycle, bounded channels, cancellation, and settings revisions.
- Implement validated/versioned settings with atomic saves.
- Add OS credential storage with session-only fallback.
- Implement memory with stable identities, turn/byte limits, and deterministic eviction.

Exit: tests establish bounded state, safe secret handling, atomic settings recovery, preview isolation, and no undelivered assistant turn committed as delivered.

## Phase 3 — Twitch OAuth and EventSub

- Implement public-client device login, token validation, serialized refresh, and rotated-token persistence.
- Connect EventSub, subscribe to channel chat, receive normalized messages.
- Handle keepalive, network backoff, directed reconnect transfer, revocation, and duplicate delivery.
- Send a deterministic test reply through Helix in an explicitly chosen development channel.
- Surface sent/rejected/unknown outcomes and the correct user-facing reconnect action.

Exit: local protocol simulations pass for welcome, timeout, Ping/Pong, normal reconnect, directed reconnect, duplicate events, authorization expiry/revocation, and delivery rejection. Complete one authorized end-to-end Twitch test before calling the connection production-ready.

## Phase 4 — AI provider adapters and reply policy

- Implement OpenAI Responses, Anthropic Messages, OpenRouter, and compatible endpoint adapters.
- Add exact model IDs, provider-specific options, deadlines, response-size limits, and normalized errors.
- Add command/mention triggers, self/exclusion checks, cooldown, bounded admission, and output cleanup.
- Connect generation → validated send → confirmed-memory-commit.
- Add a private preview path that never sends to live chat.

Exit: request/response fixture tests and mock HTTP integration tests pass for all adapters. Verify cancellation, malformed/empty output, 401/403/429, timeout, oversized body, and no hidden retries. Then perform opt-in live provider smoke tests with available credentials.

## Phase 5 — configuration experience

- Carry the accepted POC layout and visual style into the native UI under MPD Bot branding.
- Native screens: Overview/Twitch, Personality, AI Provider, Memory/Limits.
- Connect/Reconnect Twitch, provider credentials, exact model IDs, saved-state feedback, private preview.
- Tray Open, status, Pause/Resume, and Quit.
- Bounded diagnostics that distinguish generation, delivery, cancellation, and failures.

Exit: a user can complete setup, configure personality, test privately, connect a channel, hide the window, pause/resume, and quit without a terminal or editing files. Setup never exposes secrets in status/errors.

## Phase 6 — hardening and distributable builds

- Cross-platform build/test CI with a committed lockfile.
- Synthetic burst and multi-hour soak tests; verify memory plateaus and responsiveness.
- Test suspend/resume, network loss, refresh rotation failure, keychain unavailable/locked, and second-instance behavior.
- Package an executable/app for each supported OS; document Linux desktop requirements.
- Complete signing/notarization decisions before public distribution; no automatic updater in v1.
- Document real performance measurements, known limits, data flow, and recovery steps.

Exit: requirements are demonstrated on supported OS targets. Clearly distinguish simulated tests, local tests, and live service tests in the release checklist.

## Prototype disposition

The current workspace contains prematurely written provider, memory, HTTP/browser UI, and Twitch code. It has not passed the Rust build/test suite, and there have been no live Twitch or paid-provider tests. Local compiler tooling was downloaded under `.tools/`.

After architecture review:

- Inspect provider parsing/configuration ideas and tests for reuse; keep only what fits the accepted contracts.
- Replace the browser UI and local HTTP server with the chosen native UI and typed commands.
- Rework Twitch authentication to include login and refresh instead of requiring pasted access tokens.
- Separate the engine from HTTP-server state; commit memory after successful delivery.
- Validate every retained implementation with the new acceptance criteria.
- Keep the original C# reference separate and preserve attribution.

## Review findings incorporated

Three bounded read-only agent reviews covered desktop/runtime architecture, Twitch authentication/protocol lifecycle, and legacy scope. The synthesis intentionally excludes proposed legacy parity that the user has deferred.

- Desktop/runtime review: native UI/main-thread lifecycle, total byte budgets, short state access, confirmed-delivery memory commit, and explicit instance handling.
- Twitch review: public-client device login, serialized token rotation, expected Client ID validation, metadata-ID deduplication, revocation/error distinctions, and no retry after ambiguous sends.
- Legacy review: preserve core personality/provider/exclusion needs, but do not blindly copy old behavior. Existing implementation has hardcoded Twitch dispatch, prompt-only output limits, raw response logging, and ineffective per-user context usage. Voice, random replies, shoutouts, and game actions remain deferred.

No automatic port of voice, shoutouts, game actions, or Streamer.bot integration belongs in these phases.
