# Skill mechanics

This file covers what changes when the document is a Skill. Everything else about writing one is in `SKILL.md`.

Skills in this project run on Claude Code and on sstack, so write for what both support.

## What sstack does with a Skill

- It reads `name` and `description` from the header and ignores every other field, including `disable-model-invocation`.
- It lists every Skill it finds in the system prompt of every Session, so each description is permanent context load.
- The `skill` tool returns the text of `SKILL.md`, header included, without saying where the file is. A pointer to another file must therefore use the path from the repository root. A relative link gives the model nothing to resolve.
- The `read` tool stays inside the project. Sibling files work for a Skill in `.claude/skills`, and the model cannot reach them for a Skill in the person's home directory.
- A person cannot invoke a Skill by name, because a Human Command never reaches the model.

## Invocation

The model must be able to reach every Skill, so every Skill needs a model-facing `description` carrying its trigger branches. The pointer rules in `SKILL.md` apply in full.

Material that only a person ever asks for does not need to be a Skill. Put it in a plain file under `docs/`, where it costs no context load.

Reference that several Skills share can live in its own Skill or in a plain file that each one points at. `writing-docs` points at `writing-prose` this way.

## Splitting by invocation

Split off a new Skill when a distinct leading word that you use in your prompts should trigger it on its own, or when another Skill must reach it. The new description is permanent context load, so that independent reach has to be worth it.
