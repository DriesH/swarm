# Spike 4: menu bar and notifications

**Verdict: pass, with one open item.** A GPUI app can own a menu bar status item, show a GPUI popover under it, read Claude and Codex usage, and keep working with the main window closed. GPUI posts notifications with action buttons and tag replacement from a bundled `.app`. macOS denied notification permission on this machine without showing a prompt that we could see, so the click on **Open** / **Dismiss** and `on_system_notification_response` are not proven yet (see [Notifications](#notifications)).

Code: [`spikes/menubar`](../../spikes/menubar). Run notes: its README.

## Results

| Goal | Result | Evidence |
|---|---|---|
| Status item from a GPUI app | Pass | `NSStatusItem` through `objc2-app-kit`. A PNG of the button's own drawing shows the bee, the % and the dot. |
| Template icon tints for light/dark | Pass | Rendered white on the dark menu bar here. |
| Usage % and "needs you" dot | Pass | Title ` 0%`, dot drawn into a second template image. |
| Click opens a popover under the icon | Pass | Real AppKit click path (`performClick:`). The window lands 4 pt under the menu bar, centered on the icon, also on a secondary display at a negative x. |
| Popover closes on click elsewhere | Pass | App deactivation closes it (tested with `NSApp hide:`). Esc closes it. |
| Works with the main window closed | Pass | Closing the main window switches to accessory mode. The icon and popover still work. |
| Claude usage | Partial | Token read and request work; every call got 429 (see [Claude](#claude-usage)). Parser tested offline only. |
| Codex usage | Pass | `codex app-server` → `account/rateLimits/read` in about 1 s. |
| Notification with 2 actions, tag replaces | Partial | Posted twice with tag `task-42`: Notification Center holds 1. Authorization: **denied** (see below). |
| Response callback logs the action | Not tested | Needs an allowed notification and a human click. |
| Dock bounce | Unclear | `requestUserAttention` returned 0 every time, including while inactive with a Dock icon. |

Screens (rendered by the app itself, no screen recording):

- `/private/tmp/claude-501/-Users-driesheyninck-personal-swarm/8c80a07b-fb4e-4e28-9cb4-d6d60749af4d/scratchpad/menubar-shots/status-strip.png`: icon, icon with usage, icon with dot
- `/private/tmp/claude-501/-Users-driesheyninck-personal-swarm/8c80a07b-fb4e-4e28-9cb4-d6d60749af4d/scratchpad/menubar-shots/popover-main-open.png`: popover
- `/private/tmp/claude-501/-Users-driesheyninck-personal-swarm/8c80a07b-fb4e-4e28-9cb4-d6d60749af4d/scratchpad/menubar-shots/popover-stopped.png`: after **Stop all**

## Versions

| Thing | Version |
|---|---|
| `gpui`, `gpui_platform` | Zed rev `70b1b91b…` (same as spike 1), features `font-kit`, `runtime_shaders` |
| `objc2` | 0.6 (locked 0.6.5), same major as `gpui_macos` |
| `objc2-app-kit`, `objc2-foundation`, `objc2-user-notifications` | 0.3.2, the version Zed's workspace uses. 0.3.2 is also the newest on crates.io. |
| `block2` | 0.6 |
| `resvg` | 0.46, the version GPUI already builds |
| `ureq` | 3.5 |
| Claude Code | 2.1.314 |
| Codex CLI | 0.154.0 |
| macOS | 27.0.1 |

## Status item

GPUI has no status item API, so a small `objc2` bridge (about 150 lines) owns it:

- `NSStatusBar.systemStatusBar.statusItemWithLength(NSVariableStatusItemLength)`.
- The button gets an `NSImage` with `setTemplate(true)` and a title like ` 42%`, image on the left.
- Click handling: a `define_class!` target with a `clicked:` action sends into a `flume` channel. A GPUI foreground task receives it and calls `cx.update(...)`. AppKit callbacks never touch GPUI state directly.
- The icon is the bee SVG rendered by `resvg` at 36×36 px (18 pt @2x). The "needs you" dot is painted into a second image, with a clear ring so it reads over the wing. Template images are one color, so the dot is not orange. A colored dot would need a non-template image and our own light/dark handling.
- The button gets an accessibility label that includes the % and "needs you".

### Popover, not NSMenu

**Use a GPUI window** (`WindowKind::PopUp`, no title bar):

- It can use `swarm-ui` components and theme tokens for the usage bars, as the design asks.
- It shares state with the app through normal GPUI entities.
- GPUI elements already take `role` and `aria_label` (AccessKit).

What it costs, and how the spike handled it:

- **Position.** Compute GPUI bounds from the status button's Cocoa frame and its `NSScreen`, and pass `display_id`. Do not move the `NSWindow` after `open_window`: that calls back into GPUI while the app is borrowed and logs `RefCell already borrowed` three times per open.
- **Activation.** An accessory app must call `cx.activate(true)` or the panel never becomes key, and then never gets the blur that closes it. Defer the call (`cx.defer`); it calls back into GPUI.
- **Dismiss.** `observe_window_activation` closes the popover when the window stops being key. The click on the icon that caused the blur would reopen it at once, so the spike ignores a toggle within 300 ms of a close. Not tested with a real mouse.
- **Keyboard.** Esc closes it. Tab between items is not done.

An `NSMenu` gets keyboard navigation, VoiceOver and dismissal for free. But usage bars need custom `NSView`s inside menu items, drawn in AppKit, outside the design system. Use `NSMenu` only as a fallback.

### Lifecycle

- With the main window open the app is `ActivationPolicy::Regular` (Dock icon). When it closes, the spike switches to `Accessory` and keeps running. GPUI does not quit when the last window closes.
- The plan says "also when the app is closed". If that means the app process has quit, its status item is gone too. Two options for the real app:
  1. "Quit" closes windows and keeps the process as an accessory app. Simple, and one process.
  2. A small menu bar helper (login item) that talks to the daemon. Survives app quits, but it is a second GPUI app and bundle.

  Option 1 is enough for v1.

## Claude usage

Claude Code itself calls the endpoint for `/usage`. Facts from the 2.1.314 binary (parts of its JS are readable inside the executable):

- **Token location**: login Keychain, generic password, service `Claude Code-credentials`. Claude Code reads it with `security find-generic-password -a "$USER" -w -s "Claude Code-credentials"`.
- **Item content**: JSON with `claudeAiOauth.{accessToken, refreshToken, expiresAt, scopes, subscriptionType, rateLimitTier, subscriptionTypeSource}` and `mcpOAuth`. `expiresAt` is Unix milliseconds stored as a **float** (a first parse as `i64` failed). The token lives about an hour; Claude Code refreshes it.
- **No Keychain prompt**: reading through `/usr/bin/security` works without a prompt, from the terminal and from the bundled app. Probably because Claude Code created the item with that tool, so it is in the item's access list. Reading it with the Security framework from our own binary would likely prompt (not tested). The spike runs `security` with a 10 s timeout and reports "access prompt open?" if it hangs.
- **Request**: `GET https://api.anthropic.com/api/oauth/usage`, headers `Authorization: Bearer <accessToken>` and `anthropic-beta: oauth-2025-04-20`.
- **Response fields** (from the `/usage` code): `five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`, each `{utilization, resets_at}`; `model_scoped[]` with `{display_name, utilization, resets_at}`; `extra_usage.{monthly_limit, used_credits}`. `utilization` is 0–100 and can be `null`. `resets_at` goes through `new Date(...)`, so an ISO string.
- **Claude Code's own failure handling** for this endpoint: 5 s timeout; on 401 it refreshes the token and retries once; on 429 or 403 it remembers the bearer and does not ask again for `Retry-After` (default 5 min, max 1 hour); on a 401 that stays, it holds off 1 hour.

**Rate limit (finding).** We never got a 200 in this spike. Every call returned **429** with `{"error": {"type": "rate_limit_error"}}` and a `Retry-After` header:

| Call | `Retry-After` | Ends at |
|---|---|---|
| 1st (Python, about 15:03 UTC) | not printed | |
| 2nd (Python, 15:04 UTC) | 2467 s | 15:45:34 UTC |
| 3rd (spike, 15:16 UTC) | 1774 s | 15:45:34 UTC |

- The end time stayed the same, so extra calls did not push it back: it looks like a fixed window per account or token, not a penalty per call.
- The account was already limited before our first call. Claude Code sessions on this machine use the same token and also call this endpoint (for `/usage` and its limit warnings), so they share the budget with Swarm.
- No `anthropic-ratelimit-*` headers came back, so the real limit is unknown.
- **Safe polling**: at most one call per 5 minutes, always honor `Retry-After`, and save the wait to disk so restarts do not call again. Expect to be limited anyway when Claude Code is busy; that is why the status line source below should be the main one.
- The response parser is tested against a sample built from Claude Code's own field reads, not against a live answer. The spike logs the top-level keys of the first 200 it gets, to check this.

What the spike does:

- Reads the Keychain on every poll, so it picks up Claude Code's refreshed token. It never refreshes the token itself: a refresh rotates the refresh token and would break Claude Code's stored login.
- Skips the request if `expiresAt` has passed.
- At most one request per 5 minutes.
- 429/403: waits `Retry-After` (capped at 1 hour) and saves that time to disk, so a relaunch does not call again.
- 401: remembers a fingerprint of that token and waits until the Keychain holds a different one.
- No redirects (`max_redirects(0)`, HTTPS only), so the token can only go to `api.anthropic.com`. The token type has a redacted `Debug` and no `Display`.
- The popover keeps the last good numbers and shows the problem next to them ("Rate limited, retry in 24m").

### Better source: the status line

Claude Code's documented [status line input](https://code.claude.com/docs/en/statusline) has `rate_limits.five_hour` and `rate_limits.seven_day`, each `{used_percentage, resets_at}` (Unix seconds). It is there for Pro and Max, after the first API response of a session. Swarm starts every Claude agent, so it can set a status line command for them that forwards this JSON to the daemon. That gives fresh usage with no token, no undocumented endpoint and no rate limit. Use the endpoint only when no agent has run lately.

## Codex usage

How Codex gets it (openai/codex `main`, `9b8c2bb`, 2026-10-30):

- **Usage endpoint**: `GET https://chatgpt.com/backend-api/wham/usage` (`backend-client/src/client/rate_limit_resets.rs`). Headers: `Authorization: Bearer <access_token>`, `chatgpt-account-id`, and a Codex user agent. The token and account id come from `~/.codex/auth.json`.
- **Response** (`codex-backend-openapi-models`): `plan_type`, `rate_limit.{allowed, limit_reached, primary_window, secondary_window}`, each window `{used_percent, limit_window_seconds, reset_after_seconds, reset_at}`; also `credits`, `spend_control`, `additional_rate_limits`, `rate_limit_reached_type`.
- **Response headers** on model calls: `x-codex-primary-used-percent`, `x-codex-primary-window-minutes`, `x-codex-primary-reset-at`, the same for `secondary`, and credit headers (`codex-api/src/rate_limits.rs`). Only useful to code that makes model calls.
- **App server**: `codex app-server` (JSON-RPC over stdio, marked experimental) has `account/rateLimits/read`. It returns `rateLimits.{limitId, planType, primary, secondary}` with windows `{usedPercent, windowDurationMins, resetsAt}`, plus `rateLimitsByLimitId`. It also sends `account/rateLimits/updated` notifications.

**The spike uses the app server.** Codex reads and refreshes its own token, so Swarm never handles it. Sequence: `initialize` (with `clientInfo`) → `initialized` → `account/rateLimits/read`. The answer comes in about 1 s. On this account (plan `go`) there is one 30-day window (43200 min) and no secondary window; Plus and Pro have 5-hour and weekly windows, so labels come from `windowDurationMins`.

Gotcha: an app opened from Finder gets a minimal `PATH`, so `codex` (Homebrew, `/opt/homebrew/bin`) is not found through `PATH`. The spike checks the usual install folders and `SWARM_CODEX_BIN`.

## Notifications

What works:

- Inside the bundle, `NSBundle.mainBundle.bundleIdentifier` is set, so GPUI enables notifications. Under `cargo run` it logs "not in an app bundle" and posting is a no-op (no crash).
- `show_system_notification` with two actions registers a category and posts. Posting a second one with the same tag leaves one delivered notification (`getDeliveredNotifications` → `["task-42"]`).
- `on_system_notification_response` registers without errors.

What blocked the rest: **authorization goes from "not determined" to "denied" about 1 s after the first request**, with no human input. Same result:

- ad-hoc signed and signed with an Apple Development identity
- launched in the background (`open -g`) and in the foreground
- three bundle ids (`dev.swarm.spike.menubar`, `.probe`, `.signed`)

Later launches log GPUI's warning `Notifications are not allowed for this application`. It may be that macOS 27 shows the request somewhere we could not see and reports "denied" until it is answered, or that a system setting blocks new apps. We could not tell without seeing the screen; the unified log for `usernoted` was not readable.

**To finish this check, the user must once:**

1. Open System Settings → Notifications → **Swarm Spike** and turn on **Allow notifications** (style: Alerts, so the buttons stay visible).
2. Run the app and use the `notify 1` / `notify 2` script, then click **Open** and **Dismiss**. The log should show `[notify] response tag=task-42 action=open` and `action=dismiss`.
3. Optional: remove the extra `.probe` and `.signed` entries from the Notifications list.

GPUI details that matter for the real app:

- It requests `Alert | Sound` on the first post, not at launch. Ask at a moment that makes sense, for example when the first agent starts.
- It never logs "granted". Read the status yourself with `getNotificationSettingsWithCompletionHandler` to show a "notifications are off" hint in Settings.
- A click on the body gives `action_id: None`. GPUI does not ask for dismiss callbacks, so swiping a notification away is not reported.
- It shows banners while the app is frontmost. The plan's "skip when focused on that task" rule must be in our code.
- `set_app_identity` does nothing on macOS.

### Dock bounce

`NSApplication.requestUserAttention` (critical and informational) returned 0 every time: while inactive in the background, and after the app had been active and was hidden, always with a Dock icon (`Regular`). Apple documents 0 as "ignored". GPUI's `Window::request_attention` calls the same API and returns early when the window is key. Not confirmed on screen, because screen capture is not allowed here. Check by hand before relying on it.

## Bundle and signing for the real app

- Info.plist: `CFBundleIdentifier`, `CFBundleExecutable`, `CFBundlePackageType APPL`, `NSHighResolutionCapable`. `NSUserNotificationAlertStyle = alert` makes new notifications alerts by default.
- Keep `LSUIElement` off. The app has a Dock icon while a window is open and switches to accessory mode at runtime.
- Ad-hoc signing (`codesign --sign -`) is enough to build and run locally. Distribution needs Developer ID signing, hardened runtime and notarization. The notification check above should be repeated with a notarized build.
- A rebuild changes an ad-hoc signature. The "denied" state survived rebuilds here, so the setting follows the bundle id.
- `open -n --stdout/--stderr/--env` is a good way to run bundles in tests with logs and settings.

## Recommendations for `swarm-usage`

1. **Sources, in order:**
   - Claude: status line `rate_limits` from running agents (documented, free).
   - Codex: `codex app-server` `account/rateLimits/read`, plus `account/rateLimits/updated` events while an app-server runs.
   - Claude fallback: `/api/oauth/usage` at most every 5 minutes, never refreshing tokens, with the backoff rules above saved to disk.
2. **Run polling in the daemon**, not the app. The daemon outlives the app, can share one schedule across app restarts, and already sees agent events. The app only displays.
3. **Model**: per source a list of windows `{label, used_percent, resets_at}`, plan, `fetched_at`, and a `Problem` (`NotLoggedIn`, `TokenExpired`, `Unauthorized`, `RateLimited{retry_at}`, `Other`). Keep the last good data when a poll fails.
4. **Menu bar number**: the highest % across windows, because that limit stops you first.
5. **Secrets**: a token newtype with redacted `Debug`, no redirects, one fixed host per token, and never in process arguments (the spike passes the token in a header, read from a pipe).
6. **Popover**: keep the GPUI window approach; build it from `swarm-ui`, compute bounds up front, add Tab navigation.
7. **Find CLIs by path**, not `PATH`: apps started by launchd get `/usr/bin:/bin:/usr/sbin:/sbin`.
