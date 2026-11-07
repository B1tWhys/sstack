# Skill mechanics

This file covers what changes when the document is a Skill. Everything else about writing one is in `SKILL.md`.

Skills in this project run on Claude Code and on sstack, so write for what both support.

## What sstack does with a Skill

- It reads `name`, `description` and `disable-model-invocation` from the header and ignores every other field.
- It lists every Skill the model may load in the system prompt of every Session, so each of those descriptions is permanent context load. A Skill that sets `disable-model-invocation: true` stays out of the listing. The listing is capped at about 1% of the context window, and past the cap the person's Skills lose their descriptions first, which makes the model less likely to reach for them.
- The `skill` tool returns the body of `SKILL.md` without its header, along with the Skill's directory and the paths of the files in it. The `read` tool can open those files, so a pointer to a sibling file can name it relative to the Skill's directory.
- A person runs a Skill by typing `/` and its name, followed by anything they want to add. The model gets the same text the `skill` tool returns, with the person's words after it. When Skills of different Scopes share a name, each is named with its Scope, as in `personal:review`, and the bare name runs none of them.

## Invocation

A Skill the model should reach on its own needs a model-facing `description` carrying its trigger branches. The pointer rules in `SKILL.md` apply in full.

A Skill that only a person runs sets `disable-model-invocation: true`. Both sstack and Claude Code then leave it out of the listing, so it costs no context load, and its `description` becomes a one-line summary for the person. No other Skill can reach it.

Reference that several Skills share can live in its own Skill or in a plain file that each one points at. `writing-docs` points at `writing-prose` this way.

## Splitting by invocation

Split off a new Skill when a distinct leading word that you use in your prompts should trigger it on its own, or when another Skill must reach it. The new description is permanent context load, so that independent reach has to be worth it.
