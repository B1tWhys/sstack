# Tools are adapters that core is handed

The tools are adapters, as the interfaces are, but for the agent instead of the person. They live in `sstack-tools`, which depends on core, and core never depends on them. Core's `tools` module is the port: the `Tool` trait, `ToolError` and `ToolOutput`, the `Approver` and `Asker` the tools call out through, `ToolContext`, and the `ToolBox` that finds a tool by name and runs it.

Core cannot build its own tools, so whoever sets a Session up hands it a `ToolFactory`, a function from a `ToolContext` to the Session's tools. Each interface passes `sstack_tools::standard`. The factory runs once for each Session, because each Session's tools differ: `land` exists only in a worktree Session, `ask` only when someone can answer, `skill` only when Skills were found, and every tool's paths depend on that Session's Workspace. Core keeps the facts about a Session that a tool reports, such as the model serving it, in a `SharedSessionInfo` handle in the context. Core updates the handle, so it never needs to name the tool that reads it.

The alternative was a `tools` module inside core, which is what we had. It made the agent import its own adapters, and every new tool meant changing core.

A tool module in `sstack-tools` may use what the crate's root shares, such as path resolution and `parse_input`, but never another tool module. The rule for which paths are Protected Paths is the exception that stays in core, in `workspace::is_protected`, because the Sandbox enforces it too, and two copies of a security rule would drift apart.

## Consequences

Core's tests cannot use the real tools, because a dev-dependency on `sstack-tools` would give them a second copy of core's types. They use the fake `read`, `write` and `bash` tools in `tools::test_support`. The real tools are tested in `sstack-tools` and in the end-to-end tests. Core's `test-support` feature exposes those fakes, the test Approvers, git helpers and a mock container runtime, and `sstack-tools` turns it on for its own tests.
