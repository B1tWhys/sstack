# Tools are adapters that core is handed

The tools are adapters, as the interfaces are, but for the agent instead of the person. They live in `sstack-tools`, which depends on core. Core never depends on them. Core's `tools` module is the port, built around the `Tool` trait.

Core cannot build its own tools, so whoever sets a Session up hands core a `ToolFactory`, a function that makes that Session's tools. Each interface passes `sstack_tools::standard`.

The alternative was a `tools` module inside core, which is what we had. It made the agent import its own adapters, and every new tool meant changing core.

A tool module in `sstack-tools` may use what the crate root shares, but never another tool module. Protected Paths are the exception that stays in core, in `workspace::is_protected`, because the Sandbox enforces the same rule, and two copies of a security rule would drift apart.

## Consequences

Core's tests cannot use the real tools, because a dev-dependency on `sstack-tools` would give them a second copy of core's types. They use fake tools from `tools::test_support` instead. The real tools are tested in `sstack-tools` and in the end-to-end tests.
