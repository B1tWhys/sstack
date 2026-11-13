# Core is the inside, and everything else depends on it

sstack follows ports and adapters. Core (`crates/core`, the `sstack` crate) is the inside. The interfaces are adapters for the person, and `sstack-tools` is the adapter for the agent. Every adapter crate depends on core, and core depends on none of them. We chose this so that core can be read, tested and changed without knowing which interfaces or tools exist, and so that adding an interface or a tool never requires a change to core.

Core meets the interfaces at `protocol` (ADR-0018) and the tools at the port in `tools` (ADR-0017).

Inside core, modules are grouped by domain concept, after the headings in `GLOSSARY.md`. Splitting by technical layer, such as model, storage and runtime, would scatter each concept across several places, while most changes touch one concept from top to bottom.

The groups import only downwards, in this order:

1. `running_harness`, which holds every Session the program has.
2. `open_session`, which holds one Session an interface has open.
3. `agent`, which runs a Session's Turns.
4. `tools`, the port.
5. The domain groups, such as `session` and `workspace`. These may import each other.
6. `protocol`, the one leaf.

Each layer's module doc says what belongs in it.

A group's root file is a facade: private submodules, and `pub use` lines for what callers need. Modules with submodules are `foo.rs` beside `foo/`, never `foo/mod.rs`, which the clippy lint `mod_module_files` enforces.

ADR-0009 rejected a third crate for the code both interfaces share. That still stands, because shared code is domain code and lives in core. `sstack-tools` is a different case: it holds adapters, not shared code.

## Narrower decisions

- ADR-0017: read before adding or changing a tool, or changing what core gives tools.
- ADR-0018: read before adding or changing what a Session reports to an interface, or what an interface sends back.
