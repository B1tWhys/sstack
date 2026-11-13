# Plan: group core's modules and move the tools into their own crate

Core has 24 top-level modules, seven of them named `session_*`, and the tools live inside core even though they are adapters. This plan groups core by domain concept, gives the interfaces and the tools one-way dependencies on core, and moves the tools into a `sstack-tools` crate. The GUI's own layout is left for a later pass, and the CLI already nests its modules.

Read `GLOSSARY.md` and `AGENTS.md` before starting a step.

## How to run a step

1. Read this file and the code your step names.
2. Steps 1 and 2 are mechanical: behaviour, tests and snapshots stay as they were. Later steps change structure, so make them test-first where they add behaviour.
3. Run `cargo nextest run --workspace --all-features` and commit with a conventional commit message, one commit per step, or per group in step 2. The hooks must pass. Never bypass them and never push.
4. Tick the step below and write a short note under it: what the next step needs to know, and anything this plan got wrong.

The order is a default. If a step turns out to fit better elsewhere, change the order and say so in the note.

## Decisions

### Conventions

- A module with children is `foo.rs` beside `foo/`, never `foo/mod.rs`. The clippy lint `mod_module_files` is set to `deny` in `[workspace.lints.clippy]`.
- A group's root file is a facade. It holds the group's doc comment, private `mod` declarations, and `pub use` lines for the entry points, so callers write `session::SessionLog`. All other code is in submodules.
- When a module moves, every import in `cli` and `gui` changes to the new path. Nothing is re-exported at an old path.

### Crates

- `sstack` (core) is the inside of the design. It depends on no other crate in the workspace.
- `sstack-tools` is new and depends on core. Each tool is a module of its own. A tool module may use `super`, but never another tool module. What the tools share lives in its `lib.rs`: `parse_input`, the test support, and path resolution with the git-ignore check, which today is `tools::project::Project`.
- `cli` and `gui` depend on core and on `sstack-tools`. Each passes the tool factory from `sstack-tools` into core's Session setup.

### Core's layout

```
protocol.rs        leaf, imports nothing else in core: SessionReport, ReportSender, Reply,
                   ApprovalRequest, Verdict, QuestionRequest, Question, Choice, Answer,
                   Response, describe_all, AgentEvent, ToolCallSummary, TurnEnd,
                   SessionEvent, and a new replay type for SessionReport::Resumed
tools.rs           leaf port: Tool, ToolOutput, ToolError, Approver, ToolContext, and the
                   dispatch that finds a tool by name and turns ToolError into output
session/           log (was session.rs: SessionLog, Entry), metadata (was session_metadata),
                   name (was session_name), store (where Sessions live, from workspace.rs),
                   instructions (what a Session starts with, from workspace.rs)
agent/             the turn loop (was agent.rs), turn_runner, queued_prompts
connection/        switching (was connections.rs), config, credentials, provider/
commands/          human (was human_command.rs), skills
workspace/         protected (the one Protected Path rule), sandbox/, worktree
open_session/      setup (was session_setup), controller (was session_controller),
                   reporting (ReportingApprover and ReportingAsker, from session_report.rs)
running_harness/   running_harness, queue, rebuild
vim/               unchanged apart from the file convention
```

Dependencies point inward. `protocol.rs` and `tools.rs` are at the bottom, the domain groups sit above them, `open_session/` sits above the domain groups, and `running_harness/` sits above `open_session/`. No domain group imports `open_session/` or `running_harness/`.

Some placements need a small change to keep that direction:

- `Checkout`, which says whether a Session works in the main checkout or a worktree, moves from `rebuild` into `workspace/worktree`, because `commands/human` uses it.
- `config` holds user settings, which today are almost all Connections. Its doc comment says it is meant to move out into its own settings module later.
- `describe_all` stays beside the Question types in `protocol`, though only the TUI uses it today. The GUI is expected to use it too.

### The interface boundary

- `protocol` holds what passes between core and the interfaces in both directions. The vim types (`VimEvent`, `Draft`, `VimState`) stay in `vim/`, which is nvim's own channel to the interface.
- `SessionReport::Resumed` carries a replay type made for interfaces, not the log's `Entry`. The log's on-disk shape must keep reading older logs, while the messages can change with every commit, so the two get separate types.
- The person's decision on a host command (`Verdict`) and the Question data are conversation between the person and the harness, so `protocol` defines them, and `tools.rs` and the tools import them from there.

### The tool boundary

- Tools are adapters, as the interfaces are, but for the agent instead of the person. They depend inward on core.
- Core never depends on `sstack-tools`. Session setup takes a factory, something like `Arc<dyn Fn(&ToolContext) -> Vec<Box<dyn Tool>>>`, and calls it once for each Session it sets up, because each Session's tools differ. `land` is only in a worktree Session, `ask` only when someone can answer, `skill` only when Skills were found, and every tool's paths depend on that Session's Workspace.
- `ToolContext` carries what core knows about the Session: the Workspace root, the readable Skill directories, the Sandbox, the Approver, the Asker, the Skills, the worktree, the secret env, and a shared handle to the session info.
- `ToolBox::served_by` reaches into the `session_info` tool today. That reverses: core updates the shared session info handle in `ToolContext`, and the tool reads it.
- The Protected Path rule is written once, in `workspace/protected`. Both the Sandbox's masks and the tools' path resolution call it. Today `tools/project.rs` (`is_protected`) and `sandbox/mod.rs` (`masks`) each have their own copy.
- `main_checkout` in setup canonicalizes the directory itself and stops using `Project`.
- Core's tests use small fake tools from core's own test support. A dev-dependency on `sstack-tools` would give core's tests a second copy of core's types. Real tools are tested in `sstack-tools` and in the end-to-end tests.

### Docs

- ADR 0016 covers the crate architecture and dependency rules: core is the inside, the interfaces and `sstack-tools` are adapters, `protocol` and `tools.rs` are the leaves, and core is layered as above. It explains that ADR 0009's rejected "third crate for shared code" was a different case, because `sstack-tools` is an adapter, not shared code. It ends with a pointer to each narrower ADR and a line saying when to read it.
- ADR 0017 covers the tools as adapters: the port, `ToolContext` and the factory, tool modules using only `super`, the single Protected Path rule, and core's fake tools.
- ADR 0018 covers the interface boundary: `protocol`, and the replay type kept apart from the log's `Entry`.
- `AGENTS.md` gets one line: read ADR 0016 before adding a crate or top-level module, or before moving code across one. It does not point at 0017 or 0018 directly.
- Core's `lib.rs` doc describes the layering inside core.
- `GLOSSARY.md` has the term Open Session, which `open_session/` is named after.

## Steps

### [x] 1. Deny `mod.rs` files

Add `mod_module_files = "deny"` to `[workspace.lints.clippy]` and convert every `foo/mod.rs` in the workspace to `foo.rs`.

Note: six files moved. Only `sandbox.rs` needed a change, its `include_str!` of the Containerfile. Snapshots needed no move.

### [x] 2. Group core's modules

One commit per group, moving files and fixing paths only. Make each group root a facade as it is created. `protocol.rs` and `tools.rs` come later, so the modules they replace stay in place for now.

Note on `session/`: done. The facade flattens names, so functions that leaned on their module's name were renamed: `session_name::known`, `listed` and `title` became `known_name`, `listed_name` and `window_title`, and `session_metadata::relocate` became `relocate_metadata`. Where a crate already has a local module of the same name as a group (the GUI's `window::session`), import the items rather than the group.

Note on the other groups: done, one commit each. More renames for the same reason: `connections::open` and `record` became `open_connection` and `record_connection`; the Skill functions became `discover_skills`, `skill_locations`, `find_skill` and `load_skill`; the worktree functions became `worktree_path`, `worktree_branch`, `rename_worktree_branch`, `create_worktree`, `remove_worktree` and `main_checkout_of`; the Rebuild functions became `source_to_build`, `build_harness` and `relaunch_into`. `agent.rs` is `agent/turn.rs`, `turn_runner.rs` is `agent/runner.rs`, and `running_harness.rs` is `running_harness/rules.rs`. `Checkout` moved to `workspace/worktree`. `.config/nextest.toml` gives the Podman tests a longer timeout by test path, so moving a test module means updating that filter. `question` and `session_report` stay top-level until step 3, and `tools` until step 5.

### [x] 3. Extract `protocol`

Move the boundary types into `protocol.rs`, add the replay type for `Resumed`, and convert both transcripts to read it.

Note: done in two commits, the replay type first so that `protocol` could be a leaf from the start. The replay type is `Replayed`, made by `agent::replay`, which now leaves out the calls a put-off Turn never started, so `calls_not_started` is private to `agent/`. `SessionEvent` stayed in `running_harness/rules`, because no interface uses it. The `Asker` trait moved to `tools.rs` beside `Approver`, as both are ports the tools call. `ReportingApprover` and `ReportingAsker` are in `open_session/reporting.rs`. `describe_all` is used by the `ask` tool and the GUI as well as the TUI.

### [x] 4. One Protected Path rule

Add `workspace/protected` and make the Sandbox and `Project` call it.

Note: done. The rule takes a path relative to the directory that holds it, because a worktree Session's root is itself inside `.sstack`, and checking absolute paths would hide everything in it. The tools now also treat `.sstack` as protected anywhere in the Workspace, as the Sandbox did. Before, git-ignore hid it from them. `globset` is a new direct dependency, already in the lockfile through `ignore`.

### [x] 5. Invert the tool port

Make `tools.rs` the port, add `ToolContext` and the factory parameter to setup, reverse the session info update, and give core's tests fake tools. The tools still live in core at the end of this step, under a module the factory points at.

Note: done, except the fake tools, which move to step 6. Core's agent tests rely on what `read`, `write` and `bash` really do, so they keep using the real tools while those are still in core. The adapters live in `standard_tools.rs` beside `standard_tools/`, which becomes the new crate's `lib.rs` and modules. `standard_tools::standard` is the factory, and each interface passes `.tools(&standard)` to `prepare`. `ToolContext` has a `bon` builder. `SessionInfo` and `SharedSessionInfo` are in the port, so `ToolBox::served_by` updates the shared handle without naming the tool, and the `session_info` tool is now always offered. Core's tests outside `standard_tools` use `standard_tools::test_support` (`toolbox`, `git_project`, `git`, `AllowAll`, `DeferAll`), including the worktree tests' `git`. Step 6 must replace each of those in core.

### [ ] 6. Move the tools into `sstack-tools`

Create `crates/tools`, move each tool into its own module there along with the shared helpers, and make `cli` and `gui` pass the factory.

### [ ] 7. Write the ADRs and the `AGENTS.md` line

Write ADRs 0016, 0017 and 0018, the `lib.rs` layering doc, and the `AGENTS.md` pointer. Load `writing-for-agents` before editing `AGENTS.md`.
