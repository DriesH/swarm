# Only the app uses the terminal socket

The daemon's terminal socket can list, read, type into and kill every terminal, user terminals included. Agents run as the same macOS user, so file permissions alone cannot keep them out. The daemon therefore refuses any peer whose process ancestry leads back to the daemon itself (every agent and terminal program is its descendant), and the agent harness gets its own socket that never exposes user terminals (ADR 0006).

A single socket with per-request permission checks was considered. It would mix two trust levels in one protocol, and one missed check would hand an agent the user's terminals. Once Swarm ships as a signed app, the daemon also checks the peer's code signature.
