---
name: writing-for-agents
description: Writing documents for agents. Use when creating or editing a Skill, or modifying AGENTS.md or CLAUDE.md.
---

This is the reference for any document an agent consumes: a Skill, `AGENTS.md`, or a doc reached by a pointer. The aim is that the agent takes the same process on every run, which is a different aim from producing the same output.

When the document is a Skill, also read `.claude/skills/writing-for-agents/SKILL-MECHANICS.md`, which covers the header and how Skills are reached in this project.

## Context pointers

A **context pointer** is a reference in the agent's context that names some out-of-context material and encodes the condition for reaching it. A Skill's description is one. The line in `AGENTS.md` that says to read `writing-docs` before changing a doc comment is another. The pointer's wording decides when the agent reaches the material and how reliably. A must-have target behind a weakly worded pointer is a variance bug, so sharpen the wording first and inline the material only if sharpening fails.

A pointer states what the material is and lists the **branches** that should trigger reaching it. A branch is a distinct case the document handles, so different runs take different paths through it. Every word of an always-loaded pointer costs on every Turn, so prune pointers harder than bodies:

- **Front-load the leading word.** The pointer is where it does its triggering work.
- **Write one trigger per branch.** Synonyms that rename a single branch are that branch written twice, so collapse them.
- **Cut identity the body already carries.**

## The two loads

Every document and pointer you add spends one of two budgets:

- **Context load** is the cost of always-loaded material on the agent's window. An `AGENTS.md` line or a Skill description spends tokens and attention on every Turn, whether or not it fires.
- **Cognitive load** is the cost on the person, who must know which documents exist and when to reach for each. Spend it where human judgement matters and remove it where it does not.

Material behind a pointer costs only the pointer's line of context load. Material with no pointer costs only cognitive load.

## Information hierarchy

A document has two content types. **Steps** are the ordered actions the agent performs. **Reference** is definitions, rules and facts consulted on demand. A document can be all steps, all reference, or both. `writing-prose` is a short process over a long flat set of rules. The core decision is where each piece sits on the **information hierarchy**, a ladder ranked by how immediately the agent needs the material:

1. **In-file step** is the primary tier: what the agent does, in order.
2. **In-file reference** is consulted on demand. A flat set of peers, such as every rule of a review, is a fine arrangement.
3. **Disclosed reference** sits in a separate file behind a context pointer and loads only when the pointer fires. It can be a sibling file or a file anywhere in the repository.

Push too little down and the top bloats. Push too much and you hide material the agent needs. That tension is the whole decision.

**Progressive disclosure** moves material down the ladder, out of the main file and behind a pointer, so the top stays legible. Branching is the cleanest test: inline what every branch needs, and disclose what only some branches reach. In a document with steps, reference that should be disclosed buries the steps and makes attending to them a coin-flip.

**Co-location** decides what sits beside a piece once it has its tier. Keep a concept's definition, rules and caveats under one heading, so that reading one part brings its neighbours along. The document should read like documentation written for the agent. Co-location differs from duplication, which repeats one meaning in two places. Scattering fragments one meaning across many.

**Sprawl** is a document that is too long even when every line is live and unique. Attention thins across the excess. Disclose reference behind pointers, and split by branch or sequence so that each path carries only what it needs.

## Steps and completion criteria

Every step ends on a **completion criterion**, the condition that tells the agent the work is done. It has two properties:

- **Clarity** asks whether the agent can tell done from not done. A vague bound such as "understanding reached" invites **premature completion**, where the agent ends the step early because its attention has moved to being finished. The visible steps still ahead supply that pull. Sharpen the bound first. Only if the bound is irreducibly fuzzy and you observe the rush, hide the later steps by splitting the sequence. Hiding works only across a real context boundary, such as a hand-off or a subagent. An inline call leaves the later steps in context.
- **Demand** asks how much the criterion requires. "Every modified model accounted for" forces thorough work where "produce a change list" does not. Demand drives **legwork**, the digging the agent does within the work, and it applies to reference as well as steps. `writing-prose` ends on "every rule has been applied", which sets that bar for a flat list.

The strongest criteria are both checkable and exhaustive.

## When to split

Splitting one document into two spends one of the two loads, so split only when the cut earns it.

Split by sequence where the later steps tempt the agent to rush the step in front of it. Merging sequences has the reverse effect, because it exposes each step to what follows and invites premature completion.

Split by invocation as `SKILL-MECHANICS.md` describes.

## Leading words

A **leading word** is a compact concept already in the model's pretraining that the agent thinks with while running the document, such as _lesson_, _fog of war_ or _tracer bullets_. Repeated as a token, it anchors a whole region of behaviour in the fewest tokens by recruiting priors the model already holds. A coined word recruits no priors, so you pay in definition tokens for what a pretrained word gives free. Reach for an existing word first.

A leading word anchors in two places. In the body it anchors execution, so the agent reaches for the same behaviour each time the word appears. In a pointer it anchors invocation, so a word shared by your prompts, docs and code leads the agent to the material more reliably. This is why `GLOSSARY.md` terms belong in Skills.

Hunt for passages to collapse into a leading word, such as a triad spelled out at three sites or a pointer spending a sentence on one idea:

- "fast, deterministic, low-overhead" becomes _tight_, as in a _tight_ loop.
- "a loop you believe in" becomes _red_, which turns a fuzzy gate into a binary observable state.

You get fewer tokens and a sharper hook for the agent's thinking. Assume every document carries restatements that leading words can retire.

**Negation** is the failure mode beside this technique. Steering by prohibition drags the forbidden behaviour into context and makes it more available. Prompt the positive instead: "write one-line comments" states the target, and the banned behaviour is never spoken. A prohibition earns its place only as a hard guardrail you cannot phrase positively, and then it should come with the positive target.

## Pruning

- **Single source of truth.** Keep each meaning in one authoritative place, so that changing the behaviour is a one-place edit. **Duplication** costs maintenance and tokens, and inflates a meaning's rank on the ladder.
- **The environment is a source of truth too.** `committed.toml`, `.pre-commit-config.yaml`, `Cargo.toml` and `--help` output all answer questions directly. A document that restates them is a **cache**, which earns its load only when the lookup is expensive. Write down what the agent cannot find by looking: the unwritten convention, the reason behind a choice, the gotcha no config confesses.
- **Relevance.** Check that every line still bears on what the document does. A line loses relevance if it never bore on the task or if the behaviour it describes has changed. Without a pruning discipline, stale layers settle as **sediment**, because adding feels safe and removing feels risky.
- **No-ops.** Test each sentence: does it change behaviour compared with the model's default? An instruction the model already obeys pays load to say nothing, so delete the whole sentence. Settle disagreements about a no-op by running the document. The test also grades leading words. A word too weak to beat the default, such as _be thorough_, is a no-op, and the fix is a stronger word such as _relentless_.
