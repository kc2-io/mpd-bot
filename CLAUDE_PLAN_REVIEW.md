# Claude Code review of MPD Bot plan

Reviewed: 2026-09-15. Model: claude-haiku-4-5-20251001, claude-sonnet-5.

Scope: PLAN.md only, supplied directly to Claude Code CLI. Tools were disabled; Claude did not inspect source or verify live documentation. The text below is its independent review, formatted from the CLI response. Findings are reviewer recommendations, not confirmed defects. The implementation plan has not been changed in response to this review.

---

# Review: MPD Bot Next-Milestone Execution Plan

## Context
This is a requested independent review artifact of the supplied plan document, not an implementation task. No code was read or changed; the plan text was assessed on its own terms for feasibility, architecture soundness, and completeness.

---

## Overall readiness verdict

**Conditionally ready to begin Wave 0 (baseline + spike + contract drafting), not yet ready for full Wave 1 parallel implementation.** The plan is unusually disciplined for a milestone doc: it gates UI work behind a feasibility spike, freezes contracts before parallel work, assigns file ownership to prevent merge conflicts, and specifies concrete, testable acceptance criteria (including sentinel-data leakage tests and mocked refresh-race tests). However, several load-bearing technical claims are unverified assumptions, and a few operational mechanics needed for *this specific plan's own execution model* (concurrent subagents editing one repo) are underspecified. None of these are fatal to the plan's structure, but each should be resolved before Wave 1 starts, not discovered mid-wave.

---

## Prioritized substantive findings

### 1. Twitch device-flow refresh-token semantics are asserted specifically but unverified (§3 "Twitch login")
The plan states device-flow refresh tokens are "single-use and expiring after 30 days of inactivity" and cites a Twitch doc URL. This is a very specific, load-bearing claim: it directly determines the refresh manager's retry/backoff logic, the `ReconnectRequired` transition conditions in `AuthState`, and user-facing copy ("Reconnect Twitch"). If the actual current Twitch behavior differs (e.g., no inactivity expiry, different rotation rules, or scope-dependent behavior), the OAuth subagent could build correct-looking code against a wrong model.
**Consequence if wrong:** silent premature reconnect prompts, or conversely a bot that appears "connected" with a token Twitch has already invalidated, causing send failures that get misclassified.
**Correction:** treat this as an explicit verification task at the start of Wave 0 (OAuth subagent), re-derived from the live docs at implementation time, not carried forward as given fact from the plan.

### 2. Cross-thread UI update mechanism is implied but not named (§3 "Desktop and runtime", §4)
The plan specifies a main OS thread for window/tray events and a separate Tokio thread, connected by "bounded typed commands and coalesced state notifications," and separately notes Slint as the tray toolkit candidate. GUI toolkits that own their own event loop (Slint included) typically require updates originating off the UI thread to be marshaled back onto it through a specific API (e.g., an `invoke_from_event_loop`-style primitive) rather than being touched directly from a background thread.
**Consequence:** if this isn't named as part of the shared contract, the Desktop subagent and the Lead may each assume a different (or no) synchronization discipline, producing either UI-thread-affinity panics/UB or a redesign after Wave 1 has already produced code against the wrong assumption.
**Correction:** add the concrete cross-thread update primitive (verified against whichever toolkit the Wave 0 spike selects) to the Wave 0 shared-contracts deliverable, alongside `AppCommand`/`AppSnapshot`.

### 3. Subagent execution isolation is not specified (§5, throughout)
The plan carefully assigns *file* ownership per subagent to avoid conflicting edits, which is necessary but not sufficient. It does not say whether the up-to-three concurrent subagents work in separate git worktrees/branches or share one working tree, nor how/when Wave 1 patches are integrated (merge order, who resolves cross-file interface drift discovered mid-wave). Concurrent `cargo build`/`cargo test` invocations against one shared target directory on Windows commonly produce spurious file-lock failures unrelated to code correctness.
**Consequence:** without isolation, either subagents block on each other's builds (eroding the concurrency benefit the plan is designed to capture) or two agents editing the same crate simultaneously produce a tree that doesn't compile until manually reconciled, contrary to the plan's goal of "small reviewable patches."
**Correction:** state explicitly (as a Wave 0 lead decision) whether subagents use separate worktrees/branches, and define the integration order and conflict-resolution owner for Wave 1 patches before Wave 1 begins.

### 4. Acceptance criterion for "password manager active" is untestable as written (§6.A, last two bullets)
The plan correctly diagnoses (§2) that the original password-manager prompt problem was a browser-form artifact that disappears once the app is native, and correctly hedges (§6.A) that OS keychain/unlock prompts are a separate, non-suppressible concern. But the acceptance bullet ("Verify native saves with the user's password manager active") doesn't say what a pass or fail actually looks like once there's no browser involved — there's no browser "save password?" popup to check for in a native window, and third-party desktop password managers vary widely in whether/how they hook non-browser text fields.
**Consequence:** this criterion as written can't be objectively signed off during Wave 3 acceptance; someone will have to invent a pass/fail bar in the moment, inconsistently.
**Correction:** replace with a concrete, checkable behavior, e.g., "no unsolicited credential-store or password-manager prompt appears when saving non-secret settings; a credential-store prompt is expected only when replacing/removing a provider key or completing Twitch connect."

### 5. Wave 0 "gate" lacks a numeric go/no-go tied to §7's targets
§3 says the toolkit spike should fall back to egui/eframe "if the spike fails on required behavior or resource use," and §7 separately lists provisional resource targets (idle memory, CPU, startup time). These aren't cross-referenced, so the Wave 0 gate ("native shell viable") is currently a judgment call rather than a measurable pass/fail against the numbers the plan itself later defines.
**Consequence:** a marginal spike result (e.g., 60 MiB idle instead of 50 MiB) could be waved through inconsistently depending on who evaluates it, since no explicit tolerance or override process is defined.
**Correction:** have the Wave 0 gate explicitly reference the §7 targets as its acceptance bar, and state whether they're hard gates or soft targets requiring lead sign-off to override.

### 6. Chat transport (EventSub/IRC delivery mechanism) is assumed out of scope but never stated as such (§3, §6.B)
The plan's OAuth work is scoped to token acquisition/storage/refresh, and §6.B's live-check step ("receive an EventSub message and confirm a permitted test reply") implies the message-delivery transport already exists and is unchanged by this milestone. That's a reasonable inference from context ("Twitch currently accepts a pasted access token" implies a working chat path already), but the plan never explicitly states this boundary.
**Consequence:** without an explicit statement, a subagent could reasonably infer the milestone includes wiring up EventSub subscription management, which is nontrivial (session welcome/keepalive/reconnect handling) and not otherwise described anywhere in the architecture section.
**Correction:** add one sentence to §3 or §1 confirming the existing chat-delivery transport is unchanged by this milestone; only the credential lifecycle behind it changes.

---

## Decisions needed before implementation

1. **Confirm current Twitch device-code-grant refresh-token lifetime/rotation rules** directly from dev.twitch.tv at implementation time (Finding 1).
2. **Name the concrete cross-thread UI update primitive** for whichever toolkit the spike selects, and fold it into the Wave 0 shared contracts (Finding 2).
3. **Choose the Windows credential-store access method/library** (e.g., a keyring-style crate vs. direct Windows Credential Manager bindings) before the OAuth subagent starts `auth_store.rs` — the plan says "OS credential store" but not how it will be accessed.
4. **Decide subagent isolation and Wave 1 integration order** (worktrees/branches, merge sequencing, conflict owner) (Finding 3).
5. **Define the concrete pass/fail behavior for the password-manager acceptance test** (Finding 4).
6. **Tie the Wave 0 toolkit gate to §7's numeric targets explicitly**, including whether they're hard or soft gates (Finding 5).
7. **State explicitly that existing chat-delivery transport is out of scope** for this milestone (Finding 6).

---

## Assumptions / toolkit details requiring verification (not browsable in this review)

- Slint's system tray support maturity, platform coverage, and licensing/attribution terms for this use case — the plan already flags this correctly as spike-dependent and licensing-TBD; treat as unconfirmed until the spike and license review complete.
- The precise Twitch device-code-grant refresh-token behavior (single-use rotation, 30-day inactivity expiry) — Finding 1.
- Whether Slint (or the eventual fallback toolkit) requires a specific main-thread event-loop entry point that's compatible with "main OS thread owns window/tray events" as described, and what its idle/hidden power behavior actually is without custom timer plumbing.
- Twitch's exact behavior on `user:read:chat`/`user:write:chat` scope validation errors and the EventSub `channel.chat.message` transport being referenced (whether WebSocket-based EventSub is already in place, per Finding 6).

---

## Strengths

- **Contract-before-parallelism discipline**: freezing `AppCommand`/`AppSnapshot`/`AuthState`/`SafeEvent` before Wave 1 is exactly the right structure to prevent three subagents from producing incompatible interfaces.
- **File-ownership table** (§5, Wave 1) meaningfully reduces the most common class of concurrent-agent conflict (simultaneous edits to the same file), even though isolation mechanics still need specifying (Finding 3).
- **Correct, non-overpromising framing of the password-manager issue** (§2): distinguishing the browser-form mechanism from independent OS keychain prompts avoids shipping a UX promise the team can't actually guarantee.
- **Realistic legacy-credential handling**: explicitly acknowledging that pasted access tokens "cannot be converted into refresh tokens" and must remain a labeled dev-mode path avoids a common migration-plan failure mode (silently promising an impossible upgrade path).
- **Concurrency safety directives** (§4: no state locks held across provider calls/token refresh/I-O; versioned/generation-based cancellation so late results can't clobber newer state) are the correct patterns for this class of bug and are stated as hard requirements rather than aspirations.
- **Scoped, safety-conscious live testing** (§5 Wave 3, §6.B): restricting the live Twitch check to an explicitly selected dev channel with user authorization avoids the common mistake of a milestone plan casually assuming a production channel test.
