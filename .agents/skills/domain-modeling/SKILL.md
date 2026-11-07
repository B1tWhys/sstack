---
name: domain-modeling
description: Rules for sharpening the project's language and recording decisions. Use when a term is unclear or contested, or when editing GLOSSARY.md or an ADR.
---

Build the domain model while the design is being discussed, and write it down as soon as a term or decision settles. Reading `GLOSSARY.md` for vocabulary is a habit every task has, and this Skill is for changing the model.

The project has one glossary, `GLOSSARY.md` at the root, and its decisions are in `docs/adr/`.

## During the discussion

- **Challenge against the glossary.** When the person uses a term in a way that conflicts with `GLOSSARY.md`, say so at once: "The glossary defines a Turn as X, but you seem to mean Y. Which is it?"
- **Sharpen fuzzy language.** When a term is vague or overloaded, propose one precise term and name what it rules out.
- **Test with scenarios.** Invent concrete edge cases that force the boundary between two concepts into the open.
- **Check the code.** When the person states how something works, look at whether the code agrees, and point out a contradiction.

## Recording

Update `GLOSSARY.md` as soon as a term is resolved, in the format in `.agents/skills/domain-modeling/GLOSSARY-FORMAT.md`. The glossary defines words and holds no implementation detail.

Offer an ADR only for a decision that meets the test in `.agents/skills/domain-modeling/ADR-FORMAT.md`, and write it in that format.
