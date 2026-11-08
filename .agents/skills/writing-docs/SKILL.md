---
name: writing-docs
description: Rules for doc comments. Use when writing or reviewing one.
---

Write each doc comment for a reader deciding whether to use the item, who can see only its name and this comment. Every item gets one.

## Rules

1. **Intent first.** State what the item is for and why it exists.
2. **Things and actions.** A function's doc says what calling it does. Every other item's doc describes the thing itself, in whatever words read most naturally for it, so that a `pause` flag reads "Whether every Turn should stop at the end of its Step."
3. **Behavior over structure.** Describe what the item achieves, in terms that stay true if its fields or insides are rewritten. Words like "pair", "holds", "wraps" and "plus" signal a field list: restate the purpose.
4. **Callers' view.** Private items follow the same rules, written from the point of view of their caller: name the job done for the caller, and stop before how it is done. Neighboring internal concepts are fine to name; the steps are not.
5. **What the reader cannot infer.** Keep units, defaults, surprising behavior and gotchas, and state leaky abstractions plainly. For a field, give its meaning and any unit or trap.
6. **Add meaning.** A doc on an obvious item gives its role or units, so that it says more than its name and signature do.
7. **Glossary terms.** Use the terms in `GLOSSARY.md`, capitalized as they are there.
8. **`# Errors` and `# Panics`.** State the condition in caller terms.
9. **Prose.** Apply the rules in `.agents/skills/writing-prose/SKILL.md`.

## Examples

| Item | Weak | Strong |
|---|---|---|
| `sandbox` module | Running the model's shell commands where they can reach nothing but the Workspace. | A restricted environment for the model's shell commands, which can reach nothing but the Workspace, so they need nobody's approval. |
| `Connection` | A Provider paired with the name of the secret that authenticates it. | Everything needed to open a connection to a given Provider. |
| `SessionLog` | An open, append-only Session log. | The durable record of a Session, which survives crashes and can be resumed. |
| `Agent::record` | Appends an entry to the log and to the in-memory copy, keeping the two identical. | Adds an entry to the Session. |
| `Connection::secret` | The secret. | The name of the secret to look up, not the secret itself. |

## Reviewing

Test every comment against every rule. A comment passes when it would stay true after a rewrite of the internals with the same behavior.
