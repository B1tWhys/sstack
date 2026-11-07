---
name: writing-docs
description: Rules for doc comments. Use when writing or reviewing one.
---

Write each doc comment for a reader deciding whether to use the item, who can see only its name and this comment. Every item gets one.

## Rules

1. **Intent first.** State the job the item does and why it exists. A type's doc answers "what is this for?"
2. **Behavior over structure.** Describe what the item achieves, in terms that stay true if its fields or insides are rewritten. Words like "pair", "holds", "wraps" and "plus" signal a field list: restate the purpose.
3. **Callers' view.** Private items follow the same rules, written from the point of view of their caller: name the job done for the caller, and stop before how it is done. Neighboring internal concepts are fine to name; the steps are not.
4. **What the reader cannot infer.** Keep units, defaults, surprising behavior and gotchas, and state leaky abstractions plainly. For a field, give its meaning and any unit or trap.
5. **Add meaning.** A doc on an obvious item gives its role or units, so that it says more than its name and signature do.
6. **Glossary terms.** Use the terms in `GLOSSARY.md`, capitalized as they are there.
7. **`# Errors` and `# Panics`.** State the condition in caller terms.
8. **Prose.** Apply the rules in `.claude/skills/writing-prose/SKILL.md`.

## Examples

| Item | Weak | Strong |
|---|---|---|
| `Connection` | A Provider paired with the name of the secret that authenticates it. | Encapsulates everything you need to open a connection to a given Provider. |
| `SessionLog` | An open, append-only Session log. | Records a Session durably, so that it survives crashes and can be resumed. |
| `Agent::record` | Appends an entry to the log and to the in-memory copy, keeping the two identical. | Adds an entry to the Session. |
| `Connection::secret` | The secret. | The name of the secret to look up, not the secret itself. |

## Reviewing

Test every comment against every rule. A comment passes when it would stay true after a rewrite of the internals with the same behavior.
