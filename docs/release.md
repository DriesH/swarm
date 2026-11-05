# Release build

`script/bundle-mac` turns the workspace into a signed and notarized `Swarm.app` and `Swarm.dmg` for Apple Silicon. Run it from the repository root.

## Set up once

1. **Certificate.** In Xcode, open Settings › Accounts, pick the team `P2DG27TUTZ`, click Manage Certificates, and add a **Developer ID Application** certificate. It goes into your login keychain.
2. **Notary profile.** Make an App Store Connect API key (Users and Access › Integrations), then store it as a keychain profile:

   ```sh
   xcrun notarytool store-credentials swarm-notary \
     --key AuthKey_XXXXXXXXXX.p8 --key-id XXXXXXXXXX --issuer <issuer id>
   ```

   The script uses the profile `swarm-notary`. Another name goes in `--notary-profile` or `$SWARM_NOTARY_PROFILE`.
3. **Tools.** The Rust toolchain from `rust-toolchain.toml`, Zig 0.15 on `PATH`, and the Xcode command line tools.
4. **Publishing.** `gh`, signed in with write access to `DriesH/swarm` (`gh auth status`), and git-cliff (`cargo install git-cliff`).

## Run it

```sh
script/bundle-mac --adhoc     # no certificate: a test build for this Mac
script/bundle-mac             # signed and notarized
```

| Option | What it does |
|---|---|
| `--adhoc` | Signs with `-`. Skips notarization and DMG signing. Gatekeeper blocks the app on other Macs. |
| `--identity <name or hash>` | Signs with this identity. Default: the one `Developer ID Application` identity of the team. The script stops when there is none, or more than one. |
| `--notary-profile <name>` | The `notarytool` keychain profile. |
| `--no-dmg` | Makes `Swarm.app` only. |

The output goes to a new folder, `target/bundle/<version>-<UTC time>/`. The script never deletes or overwrites anything. The version is the workspace `version` in `Cargo.toml`.

## What the script does

1. Builds `swarm-app`, `swarm-daemon` and `swarm` in release mode for `aarch64-apple-darwin`, with `MACOSX_DEPLOYMENT_TARGET=13.0`. A build that is not ad hoc also sets `SWARM_TEAM_ID`, so the daemon knows its team and never skips the app socket check (ADR 0008). It stops when a binary targets another macOS or links a library the bundle does not carry.
2. Makes `Swarm.app` from `crates/swarm_app/resources/Info.plist`: the three binaries in `Contents/MacOS/`, and `Swarm.icns` in `Contents/Resources/`. The app starts `swarm-daemon` from its own folder, and the daemon finds `swarm` next to itself.
3. Draws the icon with `crates/app_icon` and turns it into `Swarm.icns` with `iconutil`.
4. Signs from the inside out, each with the hardened runtime and (not ad hoc) a secure timestamp: `swarm-daemon` as `com.swarm.app.daemon`, `swarm` as `com.swarm.app.cli`, then the app as `com.swarm.app`. Then `codesign --verify --strict --deep`.
5. Notarizes the app: a zip goes to Apple, the script waits, and on a refusal it prints the notary log and stops. Then it staples the ticket and runs `spctl`.
6. Makes `Swarm.dmg` (`Swarm.app` and an `Applications` link), signs it, and notarizes and staples it like the app.

## Check a build

```sh
codesign --verify --strict --deep --verbose=2 Swarm.app
codesign -dv --verbose=4 Swarm.app/Contents/MacOS/swarm-daemon
spctl --assess --type execute -vv Swarm.app
xcrun stapler validate Swarm.app
spctl --assess --type open --context context:primary-signature -vv Swarm.dmg
```

- `codesign -dv` shows the identifier (`com.swarm.app`, `.daemon` or `.cli`), the team, and `flags=0x10000(runtime)`. An ad hoc build shows `flags=0x10002(adhoc,runtime)`.
- `spctl` says `accepted` with `source=Notarized Developer ID`.

Then start the signed app and make sure that the daemon accepts it. The daemon log shows no "refused" line for the app. These checks need a signed build, because tests run unsigned:

- The app connects and shows its projects.
- `--identity` with an Apple Development certificate gives an app that the daemon refuses. The app socket accepts Developer ID only.

## Publish a release

A release is a GitHub release of `DriesH/swarm` (ADR 0011). Installed apps find it through its `latest.json` (Updates in `PLAN.md`).

1. On an up-to-date `main`, run `script/prepare-release 1.2.0`. It opens the PR `task(release): v1.2.0` with the new version in `Cargo.toml` and the new section of `CHANGELOG.md`. Read the section: it lists the titles of the `feat`, `fix` and `perf` PRs since the last release. Fix a title that does not read well for a user in `CHANGELOG.md` in that PR. Run it without a version to see the version git-cliff suggests.
2. Merge the PR, then pull `main`.
3. Run `script/bundle-mac`.
4. Run `script/publish-update target/bundle/<folder>`. It checks the build, tags `v1.2.0`, and publishes the release with `Swarm-1.2.0.zip`, `Swarm.dmg` and `latest.json`.

For the first release, pass `--notes-file <file>` to `publish-update` with a short hand-written note.

If `publish-update` stops halfway, the release stays a draft and the app does not see it. Fix the cause and run it again: it reuses its own draft and tag.

## Entitlements

There is none: no `Swarm.entitlements` file, and the script signs without `--entitlements`. The hardened runtime needs one only for code that the Swarm binaries do not have:

- No JIT and no executable memory that is not signed (no `PROT_EXEC` or `MAP_JIT` mapping).
- No Apple Events (`osascript`, `NSAppleScript`).
- No library loading at run time (`dlopen`, `libloading`). The binaries link system libraries only, and the script checks that.
- No camera or microphone.

Add `crates/swarm_app/resources/Swarm.entitlements` and pass `--entitlements` in `sign` when a change needs one.

## The icon

`crates/app_icon` draws every size of the icon from the sprite in `crates/swarm_sprite`, at its own size. It is the source of truth. `assets/app-icon/icon-1024.png` is a copy for review. Make it again with:

```sh
cargo run -p app_icon -- --png assets/app-icon/icon-1024.png
```

## Known limits

- Apple Silicon only. There is no Intel or universal build.
- The updater and the publish scripts have not run against a real release yet.
- No test on a clean Mac yet.
- A Finder launch gives the app a short `PATH`. Besides `PATH`, Swarm looks for agents and `gh` only in `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin` and `~/.claude/local`. A tool in another folder, such as one of a version manager, is not found. Not tried yet on a Finder launch.
- `CFBundleVersion` is the same as the version until updates need a build number.
