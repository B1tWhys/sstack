# The Sandbox keeps its build cache in a volume of its own

Cargo in the Sandbox compiled every dependency from scratch in each new container. The image now includes sccache, which caches compiled crates in a named volume the runtime keeps, mounted at `/sccache`. A new Sandbox for the same project reuses what earlier Sandboxes built.

The volume is per project, not global, and only the Sandbox reads it. The host's own builds use a separate sccache and cannot read this one. A Sandbox that wrote a poisoned cache entry could therefore affect only later Sandbox builds for the same project, which run with no network. Sharing one cache across projects would let a Session in one repo plant code that runs in another project's Sandbox, and a writable host cache would let it plant code the host's own builds link and run. Both are rejected.

A named volume stores the cache rather than a host directory, so that cache reads and writes stay inside the runtime and do not cross the VM boundary on macOS. `podman volume rm sstack-cargo-<hash>` clears one, with the hash of the project's main checkout path.

## Consequences

sccache cannot cache proc macros, build scripts, or the Workspace's own crates, so a new Sandbox still builds those. Cargo's incremental cache is also lost with `/target`, so the Workspace's crates start from nothing. Only compiled dependencies are reused.

The image must include sccache, and the image tag follows the Containerfile, so enabling the cache rebuilds the image once. The cache works with every cargo subcommand because sccache wraps the compiler itself.
