# Shell commands run in a container, not an OS sandbox

The model's shell commands run without approval inside a Sandbox: one long-lived container per Session, started on first use, with no network and only the Workspace mounted, at its host path. We rejected the OS-level sandboxes that Codex and Claude Code use, a Seatbelt profile on macOS and bubblewrap or Landlock on Linux. They share the host's kernel and user, and on macOS their safety rests on a hand-kept list of Mach services. A container on macOS runs in a Linux VM, so a command can reach nothing on the host that is not mounted.

The cost is that the host's toolchain cannot run in the Sandbox. The image is Ubuntu with the tools models reach for when searching and editing, and no compilers. Building, testing and git get their own tools that run on the host, and until they exist the model asks to run such a command outside the Sandbox, which the person approves as before.

The Sandbox cannot see Protected Paths, `.git` included, so git fails inside it on purpose. A writable `.git` would let a command plant a hook that later runs on the host, outside the Sandbox.

## Consequences

sstack drives the container through the runtime's CLI. It tries `podman`, `finch`, `nerdctl` and `docker` in that order, which all take Docker's arguments. A runtime that does not, such as Apple's `container`, needs an implementation of its own. With no runtime available, or an image that fails to build, every command falls back to asking the person, and the question says why.

A headless run has no Sandbox and still denies every shell command, so that it never depends on a container runtime.

A Rebuild replaces the process in place, so the Sandbox survives it. A container is labelled with its Session and the Harness's process id, and the next Harness to start removes any whose process has gone.

Only the files the Sandbox could see when it started are protected. A `.env` created on the host later, or one inside a directory git ignores, stays visible until the Sandbox restarts. Files git ignores are otherwise visible in the Sandbox, though the file tools hide them.
