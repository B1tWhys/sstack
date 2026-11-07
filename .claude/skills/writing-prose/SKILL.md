---
name: writing-prose
description: How to write prose in this project. Use when writing, editing or reviewing a README, an ADR, a commit message or a PR description, or when asked to unslop text.
---

Write for a tired engineer, who should understand each sentence on the first read and learn something true of this project from it.

## Process

1. Draft the text, or take the text you were given and keep its meaning and tone.
2. Test the text against every rule below and rewrite each sentence that fails.
3. Ask what still marks the text as machine-written, then fix that too.

The text is done when every rule has been applied and step 3 finds nothing.

When a rule makes a sentence worse, fix the sentence another way or leave it alone. A sentence that obeys every rule and still sounds like a machine wrote it has failed.

Terms in `GLOSSARY.md` beat these rules, so Harness and Turn stay as they are there. Skills and `AGENTS.md` also follow `writing-for-agents`, which wins where the two differ. `writing-docs` decides what a doc comment says, and these rules decide how it reads.

## Rules

### Content

1. **Superficial -ing phrases.** "highlighting...", "ensuring...", "reflecting...", "showcasing...", "fostering...". Delete them or expand them with real sources.
2. **Vague attributions.** "Experts believe", "Industry reports suggest", "Some critics argue". Name the source or delete the claim.

### Language

3. **AI vocabulary.** Additionally, crucial, delve, enduring, enhance, fostering, garner, interplay, intricate, landscape (abstract), pivotal, showcase, tapestry (abstract), testament, underscore, vibrant. Replace them with plain words.
4. **Fancy ways to say "is".** "serves as", "stands as", "boasts", "features". Say "is" or "has".
5. **"Not just X, but Y."** State the point directly.
6. **Rule of three.** Use the natural number of items, whatever it is.
7. **Synonym cycling.** Call each thing by one name everywhere, and use the name in `GLOSSARY.md` when it has one.
8. **False ranges.** "from X to Y" where X and Y aren't on a meaningful scale. List the topics directly.
9. **Plain words.** "utilize" and "leverage" become "use", "facilitate" becomes "help", "numerous" becomes "many", "in the event that" becomes "if".
10. **Real names.** Write the real symbol, file, flag or command, so that the codebase is the word list.

### Style

11. **Em dashes.** Replace each one with a period or a comma. If a thought needs separation, end the sentence.
12. **Colons.** Keep them before a list or example. Rewrite a mid-sentence connector so the point stands alone: "If you're coming from traditional automation: instead of registering event handlers, you describe conditions" becomes "Describing when the scheduler should fire works best as plain English."
13. **Boldface.** Leave proper nouns and acronyms in plain text.
14. **Inline-header lists.** The tell is a bold label and colon that restates the line: "**Performance:** Performance improved...". Convert those to prose. A bold lead-in that ends in a period, names the item, and comes before new detail is fine.
15. **Headings.** Use sentence case.
16. **Quotes.** Use straight quotes.

### Chatbot artifacts

17. **Chatbot phrases.** "I hope this helps!", "Let me know if...", "Of course!", "Certainly!", "Found the smoking gun!". Remove them.
18. **Sycophantic tone.** "Great question! You're absolutely right!". Respond directly.

### Filler

19. **Filler phrases.** "In order to" becomes "To". "Due to the fact that" becomes "Because". Delete "It is important to note that".
20. **Hedging.** "could potentially possibly be argued that it might" becomes "may".
21. **Generic conclusions.** "The future looks bright." State specific plans or facts.

### Jargon

22. **Abstract metaphor nouns.** Substrate (base), wedge, wedge in (add), vector (way, method), locus, vantage, nexus, primitive as a noun, surface as in "API surface", bedrock, scaffolding as a metaphor, modality, paradigm, gold-plating (more than the job needs), ratchet as a metaphor (a limit that only tightens), evacuate for moving code (move out), endgame (the last phase), north star, flywheel. Pick the concrete word.

### Plain speech

23. **Mechanism over feeling.** "SQL you can read" names a feeling. Name the mechanism or a number: "`.toSQL()` returns the exact string sent to the database". If a sentence could appear unchanged in another project's docs, cut it.
24. **Dense sentences.** One idea per sentence. If the reader has to backtrack to parse a sentence, split it or drop clauses.
25. **Active voice.** Name the actor: "queries are validated" becomes "the compiler validates queries". Keep the passive only when the actor is unknown or doesn't matter.
26. **Adverbs.** Use a stronger verb or the number: "significantly improves" becomes the measured delta.
27. **Mannered prose.** Replace aphorisms, rhetorical fragments, personified code ("the plan holds it") and figurative verbs ("rides along") with the literal phrase. "A dial worth turning" becomes "a parameter worth varying".
28. **Over-compression.** Write whole sentences with their articles and verbs, and spell out arrows and abbreviations. "Parser rejects bad date → exit 2, no write" becomes "The parser rejects a bad date, exits with code 2, and writes nothing."
29. **One reading.** Make every "it", "they" and "this" point at one obvious thing, repeating the noun when in doubt. Keep "only" and "not" next to the word they change.
30. **Rhythm and view.** Mix sentence lengths: short ones state a point, and longer ones carry a fact with its condition. Where the text weighs a trade-off, as an ADR does, say what you make of it.

## Example

Before:

> Additionally, Rebuild serves as a crucial mechanism for seamlessly leveraging your latest changes — ensuring that the Session is preserved in the event that the build is successful.

After:

> Rebuild builds the Harness Source and relaunches into the same Session. If the build fails, the Running Harness keeps going.
