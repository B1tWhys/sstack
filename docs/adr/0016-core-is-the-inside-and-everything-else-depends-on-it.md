# Core is the inside, and everything else depends on it

sstack follows ports and adapters. Core (`crates/core`, the `sstack` crate) is the inside. The TUI and headless mode (`sstack-cli`) and the GUI (`sstack-gui`) are adapters for the person. The tools (`sstack-tools`) are adapters for the agent. Every adapter crate depends on core, and core depends on none of them. We chose this so that core can be read, tested and changed without knowing which interfaces or tools exist, and so that adding an interface or a tool never reaches into core.

Core meets the interfaces at `protocol`, which imports nothing else in core (ADR-0018). It meets the tools at the port in `tools` (ADR-0017).

Inside core, modules are grouped by domain concept, after the headings in `GLOSSARY.md`, rather than by technical layer. A layer split such as model, storage and runtime would have scattered each concept: a Session's log, its directory, its Turns and its setup would have sat in four places, while most changes touch one concept from top to bottom. The groups import only downwards through these layers:

1. `running_harness`: every Session the program holds, what quitting or a Rebuild does to all of them, and the Rebuild itself.
2. `open_session`: setting one Session up, what an interface keeps for each Session it has open, and what that Session is doing.
3. `agent`: the loop that runs a Session's Turns.
4. `tools`: the port the agent calls tools through.
5. The domain groups: `session`, `connection`, `commands`, `workspace` and `vim`. These may import each other. `session` and `connection` do, because naming a Session needs a Provider and a Provider reads Session entries.
6. `protocol`, the one leaf.

A group's root file is a facade. It holds the group's doc comment, private `mod` declarations and `pub use` lines for its entry points, so callers write `session::SessionLog`, and the first screen of the file is the group's public API. Modules are `foo.rs` beside `foo/`, never `foo/mod.rs`, which the clippy lint `mod_module_files` enforces.

ADR-0009 rejected a third crate for the code both interfaces share. That still stands: shared code is domain and lives in core. `sstack-tools` is a different case, because it holds adapters, not shared code.

## Narrower decisions

- ADR-0017: read before adding or changing a tool, or changing what core gives tools.
- ADR-0018: read before adding or changing what a Session reports to an interface, or what an interface sends back.
