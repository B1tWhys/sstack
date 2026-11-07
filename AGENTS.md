# sstack

Use the vocabulary in `GLOSSARY.md`. Design decisions are in `docs/adr/`.

## Setup

```sh
cargo install --locked cargo-llvm-cov cargo-crap committed
brew install prek
prek install
```

## Rules

- All tech choices must support macOS and Linux. Only macOS is tested for now.
- Commit to `main` after each working change, using conventional commits.
- Never push without asking.
- Write doc comments on every item, private ones and fields included, even when the name makes the purpose obvious. They will be parsed later, so this overrides any general advice to keep comments minimal. Lints enforce it.
- Never bypass the hooks. Every function must have a CRAP score of 6 or less.
