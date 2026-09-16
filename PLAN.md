# MPD Bot — desktop milestone and remaining acceptance

Status: **implementation delivered; live and broader platform acceptance remains**. Updated 2026-09-15.

[ARCHITECTURE.md](ARCHITECTURE.md) describes the implementation. [README.md](README.md) describes setup. [VALIDATION.md](VALIDATION.md) records the exact checks, results and limitations; this plan does not treat pending checks as completed.

## 1. Requested milestone

| Request | Implemented result | Remaining acceptance |
| --- | --- | --- |
| Desktop UI with Windows tray minimize | Slint native window using Winit/software rendering; Minimize/Close hide; Open restores; Pause/Resume and Quit | Broader scaling/accessibility, suspend/resume and long-stream checks |
| Proper OAuth from the UI | Public-client device authorization, account validation, rotating refresh credentials, app-owned versioned credential files, cancellation/disconnect/reconnect | Live consent, chat and restart validation |
| Replace tagline | **A twitch bot with real personality** | Included in native presentation |
| Replace sidebar label | **Control Room** | Included in native presentation |
| App log view | Bounded sanitized session log, filters/search, follow/pause, clear, text selection/copy and export | Extended flood/interactive performance observation |
| Password-manager prompts on Save | Native settings controls replace browser forms | Verify behavior with the user's password manager; no OS credential-vault backend or storage checkbox |

The supplied MPD logo and cyan/slate theme, existing provider support, personality settings and private preview are preserved. Browser configuration routes, session links and status polling are retired. `--data-dir` remains; `--port` reports that it is obsolete.

## 2. Subagent execution completed

The lead maintained shared interfaces, dependency files and integrated build ownership. Up to three bounded subagents worked alongside that integration work; this avoided independent edits to shared manifests and startup logic.

| Workstream | Ownership and delivered work |
| --- | --- |
| Lead/foundation | Application extraction, bounded commands/snapshots, admission/revisions, startup/instance lock, settings migration, secrets, memory delivery boundary, build/CI integration and review fixes |
| Desktop subagent | Native feasibility shell, Slint view crate and forms, software renderer, window/tray lifecycle, draft handling, OAuth presentation, Logs and About |
| OAuth subagent | Device flow, validation, refresh/rotation serialization, credential storage, cancellation/identity races and mocked lifecycle tests |
| Diagnostics subagent | Typed bounded log ring, provider error classification/redaction, transport integration, delivery/cancellation review and documentation |

### Integration decisions

- Selected Slint 1.17.1 with software rendering and built-in tray after the Windows shell work.
- Kept one application process with main-thread UI and a separate single-thread Tokio runtime.
- Replaced the browser API boundary with typed commands; removed dependence on the local HTTP port for instance detection.
- Changed conversation memory to commit only after `is_sent: true`. Generated, rejected, cancelled and unknown-delivery replies do not commit a turn.
- Replaced session-long pasted tokens with current OAuth credentials for each Helix operation. Access-only legacy behavior requires an explicit development flag.
- Added generation checks around configuration changes, sending and credential adoption; reviewed startup, save, pause, disconnect and refresh races.
- Kept diagnostics as fixed summaries plus approved bounded fields, with no automatic log persistence.
- Applied the clarified storage requirement: API keys and Twitch access/refresh credentials persist in app-owned versioned JSON under the selected configuration directory. Native saves have no storage checkbox; the OS credential-vault backend is removed.

The implementation was reviewed across workstreams. The integrated automated tests and Windows release smoke checks passed at handoff; final command output and limitations belong in the validation report, not inferred from this schedule.

## 3. Maintainer input for live OAuth

The maintainer supplied MPD Bot's registered Twitch Client ID. It is configured in `.cargo/config.toml` for normal builds. The Twitch application must have Client Type **Public**. `MPD_BOT_TWITCH_CLIENT_ID` remains a build-time/runtime development override; no client secret is used.

The app then opens Twitch consent from Connect Twitch, obtains the account identity and required `user:read:chat` / `user:write:chat` scopes, and maintains rotating credentials. The browser is used for Twitch consent only. Users of a configured release do not need a developer application or pasted access/refresh tokens.

The Client ID prerequisite is configured. Mock tests validate error/state transitions and concurrency; live authorization and renewal still require account consent and testing.

## 4. Acceptance work still to complete

### A. Live Twitch and credential lifecycle

- [x] Configure the maintainer-supplied Twitch application Client ID in normal builds.
- [ ] Authorize the intended bot account using the desktop flow; verify connected identity and channel selection.
- [ ] Restart and restore the stored OAuth connection.
- [ ] In an explicitly selected development channel, receive a trigger and confirm one authorized test reply through EventSub/Helix.
- [ ] Observe live expiry/renewal or deliberately exercise a controlled refresh; label this separately from mocked rotation tests.
- [ ] Check reconnect recovery after network loss and account revocation.
- [ ] Verify credential-file permission failures and interrupted-write recovery on the reference machine.
- [ ] Verify saved provider keys and Twitch authorization restore from `credentials` after restart, including a custom `--data-dir`.

### B. Windows product acceptance

- [ ] Complete keyboard navigation, focus, small-window and 100/150/200% scaling review.
- [ ] Verify native settings/key saves with the user's password manager active.
- [ ] Exercise suspend/resume, Explorer/tray restart, and missing-tray fallback beyond the release smoke check.
- [ ] Confirm unsaved edits, pause/resume, Quit and second-launch behavior across the final packaged build.
- [ ] Check private preview against configured providers when authorized to make billable test requests.

### C. Resource measurements and portability

- [x] Record reference machine, OS, renderer and build settings in VALIDATION.md.
- [x] Measure the isolated full native UI/tray fixture hidden over ten minutes (35.42 MiB peak resident; no measurable CPU-time increase).
- [ ] Measure the connected application, open settings/Logs, slow provider response and network failure.
- [ ] Run a long-stream/synthetic flood soak and verify bounded memory/no delayed reply backlog.
- [ ] Evaluate provisional targets: hidden resident memory below 50 MiB, open below 100 MiB, hidden CPU below 0.1% of one logical CPU, startup below two seconds excluding network login.
- [ ] Run macOS/Linux CI build/tests and separately test desktop/tray/runtime behavior on those systems.
- [ ] Record package size and third-party notices for the distributed artifact.

The targets are acceptance proposals, not guarantees. Existing fixture/smoke results must be reported under their actual conditions; they do not close the connected-bot or soak gates. Windows is the current runtime-tested platform. Linux tray support depends on the desktop environment.

## 5. Follow-up execution schedule

Use the same bounded ownership pattern for acceptance:

1. **Lead:** verify live sign-in with the configured Client ID, prepare the final release and keep the validation report authoritative.
2. **OAuth subagent:** review live-login/refresh observations and handle narrowly reproduced credential failures.
3. **Desktop subagent:** verify scaling, tray fallback, draft handling and password-manager behavior.
4. **Diagnostics/transport subagent:** review resource/flood evidence, log redaction and delivery outcomes.
5. **Lead:** resolve concrete findings, rerun affected checks and record the final limits before release.

Independent checks can run in parallel. Builds sharing a target directory and edits to shared startup/manifests remain coordinated by the lead. Do not run live chat sends, paid provider calls or account consent merely as part of routine automated tests.

## 6. Boundaries retained

This milestone adds no Streamer.bot bridge, voice/transcription, shoutouts, random chat replies, game events, extra chat platforms, multi-channel routing, auto-start or auto-update. Those features should receive their own scoped plans after desktop and OAuth acceptance.

Existing POC settings retain their directory and schema migration: unversioned settings load as schema version 1 and are versioned on save. Credential persistence now uses `<config-directory>/credentials/provider-{id}.json` and `twitch-{clientid}.json`. These versioned JSON files are unencrypted, atomically replaced and kept separate from settings/logs. Unix restricts files to `0600` and directories to `0700`; Windows replaces the access list with current-account and SYSTEM permissions using a hidden PowerShell/.NET helper.

There is no automatic import from the earlier OS credential store. Existing users enter API keys and Connect Twitch once in the new build; later restarts restore the app-owned files. Environment API keys still override files at startup. `--data-dir` selects a separate credential directory, with the global real-instance lock retained. `MPD_BOT_LEGACY_TWITCH=1` remains an explicit access-only environment-token fallback and cannot create refresh credentials.

Historical validation/review reports describe the code tested at the time. They must not be interpreted as evidence for the new file-storage backend; the current validation report tracks its checks separately.

## Official references

- [Slint tray support](https://docs.slint.dev/latest/docs/slint/reference/window/systemtrayicon/)
- [Slint licensing and attribution](https://slint.dev/terms-and-conditions)
- [Twitch public device OAuth](https://dev.twitch.tv/docs/authentication/getting-tokens-oauth/#device-code-grant-flow)
- [Twitch validation](https://dev.twitch.tv/docs/authentication/validate-tokens/)
- [Twitch refresh guidance](https://dev.twitch.tv/docs/authentication/refresh-tokens/)
- [Twitch Send Chat Message](https://dev.twitch.tv/docs/api/reference/#send-chat-message)
## Named API profiles

Implemented named profiles with independent provider/model/endpoint/key combinations, including multiple profiles for the same provider. The UI supports create, Save & use, switch, rename, discard, remove key and confirmed deletion. Shared personality/Twitch/limit settings remain separate. The active selection and full profile records use one atomic app-owned credential file, bounded to 16 profiles and 64 KiB. Legacy configured providers are imported once; old provider files remain untouched. Profile changes cancel unsent work and clear conversation memory. Migration, key isolation, request routing, failed-write preservation and native editor callbacks have automated coverage; live provider and cross-platform acceptance remain pending.


## Chatter profiles

Implemented saved chatter profiles with nickname/description, Sarcastic/Praise/Hero/Regular styles, persisted previously seen usernames and Never respond. Typed policy guards protect provider admission and delivery; profile context is isolated to the current chatter. See [CHATTER_PROFILES_PLAN.md](CHATTER_PROFILES_PLAN.md) for the design, subagent execution outcome and remaining acceptance, and [VALIDATION.md](VALIDATION.md) for checks.
