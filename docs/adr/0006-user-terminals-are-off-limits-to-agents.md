# User terminals are off-limits to agents

A terminal the user opens belongs to the user alone. No agent can list, read, type into, resize or kill it, and the layout engine never resizes it: other tiles reflow around it. Letting the orchestrator read user terminals was considered (it could react to a failing command), but the user needs one place in Swarm that agents cannot touch, and secrets typed there must never reach an agent.
