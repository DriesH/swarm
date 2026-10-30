# Menu bar spike

Throwaway spike: a menu bar status item with Claude and Codex usage, a GPUI popover, and native notifications from a GPUI app. Result and notes: [`docs/spikes/menubar.md`](../../docs/spikes/menubar.md).

## Build and bundle

GPUI only turns on system notifications inside an app bundle, so build the `.app`:

```sh
./bundle.sh                       # build/SwarmMenubarSpike.app, ad-hoc signed
./bundle.sh --features snapshot   # adds the snap-popover script command
```

`BUNDLE_ID=... ./bundle.sh` changes the bundle id (default `dev.swarm.spike.menubar`).

## Run

```sh
open build/SwarmMenubarSpike.app
```

- A bee with a usage % appears in the menu bar. Click it for the popover.
- Close the main window: the app keeps running without a Dock icon, and the menu bar icon still works.
- **Open Swarm** brings the main window back. **Quit Swarm** quits.

`cargo run --release` also works, but without notifications.

To see logs, launch with `open -W -n --stdout log.txt --stderr log.txt build/SwarmMenubarSpike.app`.

### Environment

| Variable | Use |
|---|---|
| `SPIKE_STATE_DIR` | Folder for the Claude 429 backoff, so a relaunch does not call the endpoint again |
| `SPIKE_SCRIPT` | Script file for scripted runs (see below) |
| `SPIKE_SNAPSHOT_DIR` | Where `snap-*` commands save PNGs |
| `SWARM_CODEX_BIN` | Path to `codex`, if it is not in a usual place |

Pass them with `open --env NAME=value`.

## Usage sources

- **Claude**: reads Claude Code's OAuth token from the login Keychain item `Claude Code-credentials` through `/usr/bin/security`, then calls `GET https://api.anthropic.com/api/oauth/usage`. At most one call per 5 minutes; a 429 backs off for `Retry-After`.
- **Codex**: runs `codex app-server` and asks `account/rateLimits/read`. Codex handles its own token.

The spike never prints, logs or saves a token.

## Scripted runs

One command per line, `#` starts a comment:

```
wait 1500
click            # clicks the status item through AppKit
snap-status a    # PNG of the status item button
snap-popover b   # PNG of the popover view (needs --features snapshot)
needs-you on
notify 1         # notification with Open / Dismiss, tag task-42
notify 2         # same tag: replaces the first one
notify-status    # logs authorization status and delivered notifications
bounce           # dock bounce; `bounce info` for a single bounce
hide             # deactivates the app; the popover closes
close-main
stop-all
quit
```

## Not done

Popover keyboard navigation beyond Esc, real agent data, and a menu bar helper that outlives the app.
