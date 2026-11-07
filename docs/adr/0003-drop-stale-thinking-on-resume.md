# Drop stale thinking rather than freeze the prompt and tools

Claude ties each thinking block to the system prompt, tool definitions and messages before it, so a Session fails to resume if any of those change. A Rebuild changes them routinely, and so does editing `AGENTS.md`, because it is part of the system prompt. We ask the API to drop the blocks that no longer match, so that a resumed Session continues with the current prompt and tools. Self-improvement only works if a Rebuild takes effect in the running Session, which matters more here than keeping old thinking.

This is the current reasoning, not a settled preference, and it is open to change.

## Considered Options

- **Freeze the prompt and tool definitions in the Session and replay them.** This is what Anthropic's docs recommend, and it keeps all thinking and cache hits. We rejected it because the model would keep seeing the old instructions and tool descriptions until a new Session, even though the code had changed.
- **Leave the check on and fail.** This is what first happened, and it stranded the Session.

## Consequences

When the prompt or tools change, the model loses its earlier thinking from that Session, while its messages and tool results stay. That request also misses the cache, so it costs more once. Changing the choice later needs no migration of existing Sessions, but freezing would add the prompt and tool definitions to the Session log. The Provider boundary keeps this setting inside the Claude adapter.
