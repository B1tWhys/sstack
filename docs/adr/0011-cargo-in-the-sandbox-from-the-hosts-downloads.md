# Cargo runs in the Sandbox, offline, from the host's crate downloads

The model builds and tests Rust in the Sandbox without asking. The image installs rustup with stable and nightly, and cargo runs offline. The host's `registry` and `git` directories from cargo's home are mounted read-only where cargo in the container looks for them. Crate sources are the same on macOS and Linux, so one download serves both.

We rejected giving the Sandbox network access so that cargo could download into a cache of its own, lost with each container. It is simpler, but any command run without approval could then send the Workspace anywhere, and the Sandbox's promise of no network is what lets its commands skip approval. The mount is read-only because cargo on the host compiles and runs those same sources. A Sandbox that could edit them could plant a build script that runs outside it.

## Consequences

A crate the host has never downloaded cannot be built in the Sandbox. `cargo build` on macOS downloads only what the Mac build needs, so Linux-only crates such as `linux-raw-sys` are missing until someone runs `cargo fetch` on the host, which downloads for every platform. The same goes for a new dependency. Cargo on the host also deletes unpacked sources it has not used for a month, which the Sandbox cannot unpack again, and `cargo fetch` on the host restores them too.

Cargo in the Sandbox builds into `/target` inside the container, not the Workspace's `target` directory, so that a Linux build never overwrites the host's binaries, the Running Harness's included. Built binaries go with the container, and compiled dependencies survive through a cache volume of their own (ADR 0013).

The toolchains are fixed when the image is built, nightly included. A project that pins another version in `rust-toolchain.toml` cannot build in the Sandbox, because rustup has no network either.
