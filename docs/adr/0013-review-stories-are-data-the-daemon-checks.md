# Review stories are data that the daemon checks

A large diff read file by file hides how a change works, so Review can also show it as a story: flows by entry point, each a list of steps with their code. Agents write the story as JSON and post it with `story.post`, and Swarm draws it with one fixed, native layout. Agents do not generate the page itself: GPUI has no web view, and a generated page would skip the theme, the `swarm_ui` components and the accessibility rules.

A step points to line ranges at one commit, so the code it shows comes from the real diff, never from text the agent copied. The daemon refuses a story that leaves a changed line out: every changed line is in a step, or in **Other changes** with a reason, so a story cannot hide code. Approve still uses the Merged fingerprint, and step checks do not count toward it.

Stories exist only for Merged, because the Live diff changes under them while workers edit. A fresh worker writes the story when the other workers are done, so it describes what the code does, not what the workers meant to do. Workers adding steps as they commit was rejected: a story follows the finished code from its entry points, which no single worker sees.

Spec: `docs/specs/plan-grilling-and-review-stories.md`.
