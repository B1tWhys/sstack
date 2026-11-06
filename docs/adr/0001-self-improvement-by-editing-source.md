# Self-improvement by editing source, not plugins

sstack improves itself by having the agent edit the Harness Source in place, committing often, with a person choosing when to rebuild and relaunch into the result. We rejected the plugin architecture deepseek-harness uses for runtime extension: a plugin host would have to exist before the Harness could improve anything, which conflicts with starting minimal. We also rejected a worktree-and-gate promotion flow, because a Running Harness is unaffected by source edits until it is rebuilt, and git already gives us recovery.

## Consequences

Changes take effect only after a rebuild and relaunch, and adding a Provider is a code change. Recovery from a build that crashes on launch happens outside the Harness, with `git revert` and `cargo build` from a terminal.
