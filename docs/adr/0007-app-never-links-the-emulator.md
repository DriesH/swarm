# The app never links the terminal emulator

Terminal emulation (`libghostty-vt`) lives in `swarm_term_vt`, which only the daemon and the gallery link. The app paints rows that are already resolved to colors (`swarm_term`) with a view that knows nothing about libghostty (`swarm_term_view`). The emulator and the view only meet through a channel pair of plain data types, so the same view works in-process (gallery) and over the daemon socket.

One crate with both the emulator and the GPUI element was simpler, but every app build would then need Zig 0.15 and a Ghostty checkout, and the app could drift into reading emulator state that only the daemon has after a restart (ADR 0001).
