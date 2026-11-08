# Each interface chooses its own keys

The TUI's keys are shaped by the terminal. A terminal cannot report Command, it sends Alt as Escape followed by the key, and Alt is the one modifier vim leaves free, so the TUI's global keys and its send key all use Alt (ADR-0005). The GUI has none of these limits, so it does not copy the TUI's keys, its modifiers least of all. It uses the keys a desktop app uses on its platform: Command on macOS and Control elsewhere, so Command-Enter sends the prompt and Command-N starts a new Session. The two interfaces agree on what each action does, not on which key does it.

We rejected matching the TUI key for key. A person used to desktop apps would have to learn the terminal's workarounds, such as Alt-Enter to send, in a window that has no need for them.

## Consequences

A key in one interface is no reason to bind the same key in the other, and a difference between them is not a bug. Inside Vim Input the keys are nvim's in both interfaces, because nvim does the editing.
