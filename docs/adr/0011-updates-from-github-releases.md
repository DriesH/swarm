# Updates come from GitHub Releases, notes from git-cliff

ADR 0010 chose our own updater and an S3 bucket. The updater stays as ADR 0010 describes it. The bucket goes: every release is a GitHub release of `DriesH/swarm`. Its assets are the notarized `Swarm.app` as a zip, the DMG for the website, and the manifest `latest.json`. The app reads the manifest at `https://github.com/DriesH/swarm/releases/latest/download/latest.json`.

The release notes and `CHANGELOG.md` come from git-cliff (`cliff.toml`), from the conventional PR titles on `main`.

## Considered options

- **S3 (ADR 0010):** full control of cache headers and channels, but it needs an AWS account, a bucket, credentials on the publishing Mac, and costs money per download.
- **The GitHub API (`/repos/.../releases/latest`):** gives the notes and the asset list in one call, but unauthenticated calls are rate limited per IP address. The asset download link has no such limit.
- **Hand-written release notes:** more readable, but they drift from what shipped. With git-cliff, a PR title is the note, so PR titles must describe the change for the person using Swarm.

## Consequences

- No AWS. Publishing needs `gh`, signed in with write access to the repository, and git-cliff.
- The trust still comes from Apple's code signature (ADR 0010). A changed manifest or release can at most point to another signed Swarm build.
- `latest` is the newest published release. A release is created as a draft, gets all its assets, and is published last, so the app never sees a manifest without its zip.
- Updates depend on GitHub and on the repository staying public. A private repository would need a token in the app.
- One channel, `stable`. A beta channel would need its own manifest name, for example `beta.json`, on pre-releases.
