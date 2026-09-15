# Desktop milestone validation

Latest update: app-owned credential persistence replaces the earlier OS-vault backend. See the final section for the current checks; preceding sections record earlier builds.

Date: 2026-09-15. Source is the working tree for the native desktop milestone. This report distinguishes isolated checks from live service acceptance.

## Reference environment

- Windows 11 Home, 64-bit, build 10.0.26200.
- AMD Ryzen 9 9900X, 24 logical processors, approximately 31.1 GiB usable RAM.
- Rust 1.98.1, MSVC target `x86_64-pc-windows-msvc`.
- Slint 1.17.1, Winit backend, software renderer, Fluent Dark widgets.
- Release settings: size optimization, LTO, one codegen unit, stripped symbols, abort on panic.
- Output: `target/x86_64-pc-windows-msvc/release/mpd-bot.exe`. The earlier POC executable at `target/release/mpd-bot.exe` was left in place because its process was running. The new executable is 11,998,720 bytes (11.44 MiB), SHA-256 `95259e84d3b7b7e46d99889e2c92ae1a6e144293908a88234bb703f05195cd09`.

## Automated checks

| Check | Result |
| --- | --- |
| `cargo test --locked` | 43 passed, 0 failed |
| `cargo clippy --all-targets --locked -- -D warnings` | Passed |
| `cargo fmt --all --check` and `git diff --check` | Passed |
| MSVC release build | Passed; rebuilt after final settings-limit change |
| Independent integration source review | No P0/P1 blockers reported |

Coverage includes provider wire formats and redacted HTTP errors, OAuth device login and identity validation, concurrent 401 refresh, cancellation during refresh/storage, offline credential restore, revocation, session-only storage failure, pending authorization display, prompt-memory bounds and confirmed-delivery commits, EventSub contracts/dedup/identity checks, native form conversion, settings migration and combined size limits, secret separation, responsive Pause during slow provider/storage operations, and instance locking. Tests use mock servers and disposable files/stores, without paid requests or Twitch chat sends.

## Native Windows checks

The explicit `--desktop-spike` fixture loads the same window/tray implementation without credentials, configuration loading, or networking. `MPD_DESKTOP_SMOKE=1` schedules these assertions:

- Native minimize hides the window; tray Open restores it and clears minimized state.
- Tray Pause callback, explicit hide/restore, unsaved-draft Quit guard, and Quit complete.
- `MPD_DESKTOP_FORCE_TRAY_FAILURE=1` injects the supported failure notification. The window restores, hiding is disabled, and Quit remains available.
- Seven pages were captured with Slint's own snapshot API and visually inspected: Personality, AI provider, Twitch connection, Memory & limits, Private preview, Logs, and About. About attribution sizing was adjusted afterward and rechecked.

The separate `--desktop-app-smoke` uses the production UI callbacks and application runtime with a fresh disposable demo directory. It passed Save and disk persistence/dirty acknowledgment, Pause, a session-only synthetic API key, clearing the key field, sanitized logs, invalid-form rejection, missing-registration feedback, and production Quit. It does not access real credentials or start authorization/provider calls. A bounded 20-second timeout returns a failing exit code instead of leaving a test window running.

These checks exercise application callbacks and native window lifecycle; they do not certify every mouse/keyboard interaction, clipboard integration, native export picker, password-manager product, DPI setting, Explorer restart, or suspend/resume case.

## Resource measurements

The ten-minute hidden-window measurement uses the full seven-page UI fixture and native tray in a release executable. The fixture omits the application controller, credential store and live network connections. Its numbers characterize idle UI overhead only, not a connected bot during a stream. Sampling excludes the first two startup seconds, reads Windows process CPU time/working set/private bytes at five-second intervals, and normalizes CPU against one logical processor.

| Hidden UI fixture measurement | Observed |
| --- | --- |
| Sample duration | 602.04 seconds |
| Final resident memory | 35.36 MiB |
| Peak sampled resident memory | 35.42 MiB |
| Final private bytes | 6.91 MiB |
| Process CPU-time increase | 0.00 seconds reported by Windows counters |
| Mean CPU, one logical processor | No measurable increase (reported 0.00%) |

The fixture exited normally after measurement. Zero reported CPU is limited by counter resolution; it is not a guarantee of zero work. This isolated UI result meets the provisional hidden-UI targets under these conditions. Connected-bot and open-window targets remain unverified. The measured release snapshot preceded the final smoke-harness/status/settings-limit adjustments. Raw samples are in `.test-data/idle-measurement.json`. The earlier short minimal-shell experiment is superseded for UI overhead by this full-window fixture.

## Live and platform checks pending

- The maintainer supplied the registered Twitch Client ID; normal Cargo builds now embed it. All 43 tests passed again with the ID configured. Mock OAuth checks passed; live consent, persistent restart, refresh, EventSub and Helix delivery are not claimed.
- Current desktop live AI provider requests, real OS-store failure modes, password-manager behavior, file-picker/clipboard interaction, scaling and extended Windows shell lifecycle tests.
- Connected-bot resource tests, open-window/log-flood measurements, and long-stream soak.
- macOS/Linux build and runtime acceptance. The CI matrix remains configured, with Linux desktop dependencies added, but no remote CI run is claimed for these unpushed changes.

## Reproduce isolated checks

```powershell
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
cargo build --release --target x86_64-pc-windows-msvc --locked
.\target\x86_64-pc-windows-msvc\release\mpd-bot.exe --desktop-app-smoke
$env:MPD_DESKTOP_SMOKE = '1'
$env:MPD_DESKTOP_FORCE_TRAY_FAILURE = '1'
.\target\x86_64-pc-windows-msvc\release\mpd-bot.exe --desktop-spike
```

For a hidden UI sample, remove the two smoke variables, set `MPD_DESKTOP_IDLE_SECONDS=600`, and launch `--desktop-spike`. It hides after one second and exits after 605 seconds. Optional `MPD_DESKTOP_SCREENSHOTS` selects a local screenshot directory in the six-second lifecycle mode. These explicit test switches are separate from normal application startup.

Local raw logs, screenshots, and measurement JSON are under ignored `.test-data/`; they contain synthetic test data. The existing user configuration and running POC were not used by these checks.

## Configured Twitch Client ID build

The maintainer-supplied public Client ID is embedded through .cargo/config.toml. All 43 tests passed again. The updated executable linked successfully, but Cargo could not replace the running mpd-bot.exe (Windows file lock). The completed release/deps/mpd_bot.exe was verified to contain the Client ID and copied to target/x86_64-pc-windows-msvc/release/mpd-bot-twitch.exe. This updated binary passed the production desktop callback smoke check. SHA-256: 7ac9059173f28b825248638fed6c33f52f0a018ec66df0333b0e03e113bc2985. Quit the running app before launching it. Live consent remains pending.

## MPD branding update

The supplied transparent PNG is embedded unchanged as ui/mpd-logo.png and used in the sidebar, About page, window and tray icons. ui/brand.slint centralizes cyan/slate colors and branded buttons with pointer, keyboard/focus, disabled and accessibility actions. The MSVC release rebuilt successfully with the configured Twitch Client ID. Isolated screenshots were inspected for Personality, Twitch and About; tray lifecycle and production desktop callback smoke checks passed. Resource measurements earlier in this report predate this branding update. Current output: target/x86_64-pc-windows-msvc/release/mpd-bot.exe (12109312 bytes), SHA-256 19e67afb5bbd6c3b3796f4892c9068372a1e6e018b5e096c38c2779614c2d96d.

## Latest storage implementation

API keys and Twitch access/refresh credentials now persist in versioned JSON files under the selected configuration directory/credentials. The storage checkbox and keyring dependency are removed. Provider credentials restore on startup, updates replace only their provider file, and Remove deletes the saved key. OAuth restores validated credentials and persists refresh rotations through the same protected-file helper.

The files are unencrypted JSON with account-restricted permissions, separate from config.json/logs and ignored by Git. Unix uses file0600/directory0700; Windows creates a protected current-account+SYSTEM DACL through a hidden .NET permissions helper. An explicit unwanted Windows Everyone grant was introduced on a synthetic fixture and verified removed during loading. Symlink/reparse targets and oversized/invalid credential records are rejected.

All 50 Windows tests passed; Clippy passed with warnings denied. New coverage includes provider save/reload/replace/remove, OAuth file namespaces and restart/rotated-pair persistence, corrupt credential rejection, atomic replacement and Windows access control. A bounded independent code review found no release blockers. No real API key or Twitch token was read, migrated, or used by these checks.

Existing OS-stored credentials are not imported. Users enter API keys and Connect Twitch once in the new build; later restarts restore the local records. Old OS entries remain untouched. Cross-platform runtime and live-token acceptance remain pending.
The Windows MSVC release build passed with the lockfile enforced. The rebuilt executable passed the production desktop callback smoke (including a locally persisted synthetic key) and native minimize/tray Open/Pause/Quit smoke. The AI provider screenshot was inspected: the storage checkbox is absent and the local persistence hint is visible. Formatting and diff whitespace checks passed. Current output: target/x86_64-pc-windows-msvc/release/mpd-bot.exe (12114432 bytes), SHA-256 3238456cf71aba0fd2d440ccbe1d0c9f51c2123094099438c7d8bc4e62a6a06c.
## Optional triggers, random replies and memory estimate

The Twitch settings now include an optional chat command (off by default), a leading @mention toggle (on by default), and random reply percentage (10% default, validated 0–100). Existing settings receive these defaults and keep their saved command text and conversation limits. Disabled direct triggers and other !commands are skipped. Random selection follows transport eligibility/deduplication checks; selected and explicitly requested replies still pass through the same pause, cooldown, admission, revision and confirmed-delivery path.

The memory page now says “Remembered messages per viewer” and explains that matching bot replies are retained too. Its reactive estimate covers full retained chat history using maximum-length UTF-8 text, the existing 2 MiB text cap and approximate collection overhead. App/UI, logs and active AI requests are explicitly excluded. The personality heading uses the requested “Setup your bots personality” wording.

All 55 Windows tests passed with the lockfile enforced. Coverage includes percentage boundaries, command/mention toggle behavior, legacy defaults, config/form round trips, paused/cooldown behavior and memory estimate scaling/cap/off cases. Clippy passed with warnings denied, and formatting and diff whitespace checks passed. Live Twitch traffic and cross-platform UI behavior are not claimed by these checks.
The Windows MSVC release build completed successfully after the running application was closed. Production desktop callback smoke passed, including persisted command/mention/percentage settings and a reactive zero/nonzero memory estimate. Native minimize, tray Open/Pause, restore and Quit smoke passed. Personality, Twitch connection and Memory & limits screenshots were inspected. These smoke checks used isolated synthetic settings and made no live Twitch or AI requests. Final executable: target/x86_64-pc-windows-msvc/release/mpd-bot.exe (12156928 bytes), SHA-256 5ec70d7ed0e17a136fdd01974cff18f30aa00ba74d5329f3a73c133314dbca52.
