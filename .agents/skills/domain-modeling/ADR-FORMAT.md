# ADR format

## When a decision gets an ADR

Write one only when all three hold:

1. **Hard to reverse.** Changing your mind later would cost something real.
2. **Surprising without context.** A future reader would look at the code and wonder why it was done this way.
3. **A real trade-off.** There were other options, and one was picked for specific reasons.

If a decision is easy to reverse, it will simply be reversed. If it is not surprising, nobody will ask. If there was no alternative, there is nothing to record.

Decisions that usually qualify are the shape of the architecture, technology choices that lock the project in, scope boundaries including the explicit no's, deliberate departures from the obvious path, constraints the code cannot show, and alternatives whose rejection is not obvious.

## Layout

ADRs live in `docs/adr/` as `NNNN-slug.md`, numbered one past the highest existing number. The title is a sentence-case statement of the decision.

```md
# {The decision, stated as a title}

{What the context was, what was decided, and why. Name the rejected options and the reason for each.}
```

An ADR can be one paragraph. Add a `## Consequences` section only for effects downstream that a reader would not foresee.

## What stays out

An ADR records a decision and its reasons, so it should stay true while the code under it changes. Name the pieces the decision rests on, such as the trait that is the port or the module that is the leaf. Leave out what is cheap to change, such as which types a module holds, a struct's fields, or which cases a function handles. That detail goes in the module's or the item's doc comment, which changes in the same commit as the code. If adding a type or renaming a field would make a sentence in the ADR stale, that sentence belongs in a doc comment.
