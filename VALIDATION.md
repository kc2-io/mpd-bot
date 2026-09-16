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

## Named API profile implementation

Added a separate profile editor and an atomic credentials/ai-profiles.json store with names, stable IDs, provider/model/endpoint/key records and active selection. Saving and switching apply the saved profile, while failure leaves the previous pair applied. Metadata-only snapshots carry no keys. Existing provider keys and the selected model are imported once; profile files then become authoritative. Switching provider or endpoint clears the old key unless a replacement is supplied. Profiles are bounded to 16 records and 64 KiB total.

All 62 Windows tests passed. New tests cover separate keys and models for the same provider, cross-provider profiles, active selection across reloads, rename/key retention, endpoint/provider changes, migration without reimport, corrupt-file preservation, name/count validation, deletion, and failed-write preservation. A loopback HTTP integration test selected two compatible-provider profiles repeatedly and verified the actual model field and Authorization header for each request. Clippy passed with warnings denied; formatting and diff whitespace checks passed.

The development desktop callback smoke passed create/save/switch/rename/discard/delete/remove-key flows against disposable data, including retained keys after rename and isolated keys across same-provider profiles. Native tray smoke passed and the profile editor screenshot was inspected. No real credentials or billable/live provider calls were used. Final release validation follows below.
The Windows MSVC release build passed with the lockfile enforced. The final release passed the full production desktop callback smoke (including named profile management and key isolation) and native minimize/tray/Open/Pause/Quit smoke. Final executable: target/x86_64-pc-windows-msvc/release/mpd-bot.exe (12247040 bytes), SHA-256 6c48a555ef21e9f465637ebee4a7ae667bdd2362f30df5771c6031676f54f13c. Screenshots are under ignored .test-data/profiles-release-ui. Live services and macOS/Linux runtime acceptance remain pending.

## Compact status banner

The main status banner now has a maximum height equal to its wrapped text height plus 24 pixels of padding, with vertical stretching disabled. Inspected a temporary synthetic notice at normal 1050×780 and maximized 3440×1369 window sizes; the banner stays compact in both. The temporary visual-fixture changes were restored afterward. Normal-window native tray smoke passed. The maximized fixture captured the layout but did not meet the existing timed minimize-to-tray assertion; maximized tray timing is not claimed validated by this check. Formatting passed; final release build details follow.
The final Windows MSVC release build and production desktop callback smoke passed. Updated target/x86_64-pc-windows-msvc/release/mpd-bot.exe SHA-256: 562f0e276079bbd4c0946cb0bf267449b1a1b23a1e8d6774e91ec80ca5aaea2d.

## Tag-driven Windows release process

Added .github/workflows/release.yml for pushed v-prefixed semantic-version tags. It checks the tagged source, builds Windows x64 with a static MSVC runtime, verifies the embedded version and packages an explicit executable/README allowlist with a SHA-256 checksum. A separate write-permission job verifies the tag commit and checksum, creates/resumes a draft, uploads assets, and publishes. Existing published releases are left unchanged. Prerelease suffixes are recognized separately from build metadata. RELEASING.md documents the procedure and retry behavior.

All 62 tests and Clippy passed after adding compile-time version embedding. Release tag parsing accepted stable/prerelease/build-metadata examples and rejected malformed tags, leading-zero numeric identifiers and injection/path-like inputs. Actionlint 1.7.12 passed both workflows. A Windows release with MPD_BOT_RELEASE_VERSION=0.1.0-dev.1 and static-CRT flags linked successfully; Cargo could not replace the running mpd-bot.exe, so the completed linked binary was copied to mpd-bot-release-check.exe for isolated validation. Packaging verified its --version output, rejected a mismatched requested version, and produced a ZIP with exactly mpd-bot.exe and README.txt. The ZIP checksum was independently verified, including LF/no-BOM format for Linux sha256sum. The About screenshot displayed Version 0.1.0-dev.1 and the isolated native tray smoke passed. PE imports had no VCRUNTIME140/MSVCP140 redistributable dependency. The running user's executable was not replaced.

No version tag or GitHub release was created during validation. GitHub-hosted build/upload/publication remains pending a real tagged run after these changes are committed and pushed. Local artifacts and tooling remain in ignored .test-data/.tools paths.


## Chatter profiles — local preview, 2026-09-16

Implemented the Chatters page, four styles (Sarcastic/Praise/Hero/Regular), nickname/description, persisted observed accounts and Never respond. The implementation uses stable account identity, bounded independent stores and per-request policy guards. A read-only subagent review found binding/deny/persistence races; fixes and regression coverage are included.

Final checks on Windows:

- `cargo test --locked --quiet`: **86 passed**, zero failed. Includes storage bounds/Unicode/normalization, atomic failure preservation, stable-ID rename and login reuse, pending-binding conflicts, failed save/session deny/retry, cache retention, cancellation and targeted history removal. Local HTTP integration tests verify profile context isolation, zero provider calls for denied users, cancellation during a slow request and unrelated profile edits preserving the active request.
- `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --all -- --check` and Git diff whitespace checks passed.
- `cargo build --release --locked --target x86_64-pc-windows-msvc` passed with `RUSTFLAGS=-C target-feature=+crt-static` and `MPD_BOT_RELEASE_VERSION=0.2.0-dev.1`.
- The release executable's `--desktop-app-smoke` passed native production callback flows against disposable data: chatter save/reload, all styles/deny, another manual profile after selecting one, invalid description with preserved draft, discard, Quit protection, clear seen without profile deletion, and deletion of a different profile without losing the block. Existing settings, API-profile and synthetic-key flows also passed.
- The native tray fixture passed minimize/hide, Open, Pause, restore, draft Quit protection and Quit. The rendered 1050×780 Chatters screenshot was inspected after the final layout change; Save chatter/Discard stay visible below the scroll area. Screenshot: ignored `.test-data/chatter-release/chatters.png`.
- The copied preview reports **MPD Bot 0.2.0-dev.1** through `--version`. Executable: `dist/chatter-preview/mpd-bot.exe`, 12,858,880 bytes, SHA-256 `cb3c758c568c1189aad7bcede5c0d440f07993bb21bab828538ee39e16a5629f`.
- Local ZIP: `dist/mpd-bot-0.2.0-dev.1-windows-x64.zip`, 6,802,669 bytes, SHA-256 `8680edcd89666b0cd4b69c462de80f5e19b40b83cd11901b8b1f46c8ce0b9d2b`. Includes only the executable and preview README. No tag or GitHub release was created for this feature.

No real provider credentials, paid AI requests, Twitch messages or account-consent actions were used in these checks. Stable-ID tests use synthetic identities and provider tests use loopback servers. File reload checks are not a live connected restart test. The proposed full acceptance matrix is not entirely complete: live Twitch receive/send/refresh behavior with profile edits, a connected flood/long-stream soak, incremental process-memory measurements, broader scaling/accessibility, and macOS/Linux runtime acceptance remain to be exercised. UI estimates and store limits are bounds/approximations, not measured total RSS guarantees.


## Azure signing integration — setup in progress

Prepared separate build/sign/publish jobs with OIDC scoped to the `artifact-signing` environment, Azure action commit pins, explicit executable signing, SHA-256 timestamping, and signature verification before packaging. The environment was created in GitHub with only the approved `v*` tag deployment policy. Manual dispatch against a tag is a signing-only run; publication remains exclusive to tag pushes.

Actionlint passed, both PowerShell scripts parsed, and the signature gate rejected an unsigned MPD Bot preview before creating a package directory. It accepted the existing Microsoft-signed/timestamped PowerShell executable and rejected a disposable copy with a modified byte. These are local verification tests, not proof that Azure signing works.

Azure CLI sign-in succeeded. The existing Basic signing account in West US 2 was verified ready, with no certificate profiles. A dedicated single-tenant signing application/service principal and GitHub-environment federated credential were created without a client secret. Five non-secret environment variables (client, tenant, subscription, account and endpoint) are configured. The user received the Identity Verifier role scoped to the signing account to enable portal validation; their existing subscription Owner role was unchanged. No signing permission has been granted to the GitHub identity yet.

The user has not started Public Trust identity validation. Completing it in the Azure portal is the prerequisite for creating the certificate profile, granting the profile-scoped signer role and setting AZURE_SIGNING_PROFILE. No signing workflow has been run or new release tag created. Do not merge/ship these workflow changes until that configuration is complete; missing configuration intentionally blocks signing and publication.


### Azure setup completed — 2026-09-16

After the user's identity validation completed, Azure accepted creation of `mpd-bot-public` in the existing `MPD-Artifacts` account. Provisioning is Succeeded, profile type is PublicTrust, and status is Active. The publisher is Kenneth Caruso; optional street-address/postal-code inclusion is disabled.

Granted the GitHub service principal only the Artifact Signing Certificate Profile Signer role at this certificate profile's scope. All six environment variables are populated. Readback confirmed the GitHub deployment policy still permits only `v*` tags, and the Azure federated credential targets precisely the repository's `artifact-signing` environment with the Azure token-exchange audience. No client secret or private signing key is stored in GitHub.

The configuration prerequisite is complete. Existing local signature-gate/actionlint checks remain valid; no application code changed. A live GitHub OIDC login/sign/package run remains untested until a new tag containing this workflow is run. No new release tag was created by setup.
