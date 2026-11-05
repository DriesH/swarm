# Swarm updates itself from a manifest on S3

The source changed: updates now come from GitHub Releases (ADR 0011). The updater itself is as described here.

Swarm checks a small JSON manifest in an S3 bucket, downloads the new notarized build, checks its hash and its code signature, and swaps the app bundle when the app quits. We wrote this updater in Rust and did not use Sparkle. Sparkle brings an embedded framework with its own XPC services, and the release rules allow system libraries only (`docs/release.md`). Its update window is also AppKit, not our design system. The updater uses macOS networking (`NSURLSession`) and Security.framework, so the bundle still links system libraries only.

## Considered options

- **Sparkle 2:** delta updates and a finished update window. It breaks the system-libraries rule, it needs a second signing key (EdDSA) besides Developer ID, and its UI does not match the app.
- **A Rust HTTP client (reqwest or ureq):** less FFI code, but it puts a second TLS stack in the binary and ignores the system proxy settings.

## Consequences

- The trust comes from Apple's code signature, not from the manifest: the app installs a build only when its signature belongs to our team (`P2DG27TUTZ`) and the bundle ID is `com.swarm.app`. A changed manifest can at most point to another signed Swarm build, and the app never installs a version lower than or equal to its own.
- An update also replaces `swarm-daemon` on disk, but the running daemon keeps its version until it restarts. A restart ends every agent terminal (ADR 0001), so the app restarts the daemon only when no agent is working, or when the person confirms.
