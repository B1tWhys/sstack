# Interactive mode is an event-driven terminal UI built from components

Interactive mode is a full-screen terminal UI built with ratatui, so that the Harness can grow views beyond a single scrolling stream of text. Everything that happens reaches one `App` as an event on a single channel: key presses, the agent's progress through a Turn, and questions about shell commands. The App passes each event to self-contained components, which keep their own state and ask the App for anything beyond themselves by returning actions. The agent runs Turns on its own task, so the screen stays live while it works. We rejected having components call each other or the agent directly, because each new part of the screen would then need to know about the others.

Headless mode (`--prompt`) stays plain text on standard output, because it is meant for scripts and pipes.

## Consequences

Interactive mode now needs a terminal, and it refuses to start without one rather than take over a terminal it was not given. Interactive behavior is therefore tested in-process against a test backend, and no end-to-end test drives the interactive binary. Adding a part of the screen means writing a component and giving it a place in the App's layout and key order. A new source of events only needs a clone of the event sender.
