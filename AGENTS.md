# sstack

Use the vocabulary in `GLOSSARY.md`. Design decisions are in `docs/adr/`.

## Setup

```sh
cargo install --locked cargo-llvm-cov cargo-crap4rust committed
brew install prek
prek install
```

## Rules

- All tech choices must support macOS and Linux. Only macOS is tested for now.
- Commit after each working change, using conventional commits. In a Session with its own worktree, then call the `land` tool; otherwise commit to `main`.
- Never push without asking.
- Write doc comments on every item, private ones and fields included. They will be parsed later, so this overrides any general advice to keep comments minimal. Lints enforce it. Before writing or changing one, read `.claude/skills/writing-docs/SKILL.md`.
- Never bypass the hooks. Every function must have a CRAP score of 6 or less.
- Give a function that outgrows the argument limit a builder with `bon`; each parameter stays a parameter of its own, never grouped into a tuple.
