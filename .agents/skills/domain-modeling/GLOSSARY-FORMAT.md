# Glossary format

`GLOSSARY.md` opens with the project's name and a sentence on what it is, then a `## Language` section. Terms sit under `###` subheadings that group related terms.

Each term looks like this:

```md
**Turn**:
One drain of admitted input in a Session, ending when the model and its tools have stopped.
_Avoid_: Round, exchange
```

## Rules

- **Pick one word.** When several words name a concept, choose the best one and list the rest under `_Avoid_`.
- **Keep definitions short.** One or two sentences that say what the thing is, not what it does.
- **Only this project's terms.** A general programming idea, such as a timeout or an error type, stays out even if the code uses it everywhere.
