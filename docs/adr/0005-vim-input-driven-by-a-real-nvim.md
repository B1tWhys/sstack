# Vim Input is driven by a real nvim that pushes its state

The person types into an `nvim --embed --headless` running behind the Harness, so their own mappings, options and text-editing plugins work, and the Harness implements no vim behavior. We rejected simulating vim, because a simulation is never finished and could not run a person's config.

The engine lives in `sstack::vim`, free of any UI type, so that a future GUI can reuse it: a front end sends keys in nvim's key notation and draws the `VimState` that comes back.

Only msgpack-RPC notifications are sent to nvim, and a script running inside it pushes state from autocmds. We rejected reading state back with requests, because nvim answers no ordinary request while it waits on a key, such as the one after `f`, and the Harness has to stay free to send that key. It also needs no sleeps to let input settle, which reading state back relies on.

We wrote a small client on `rmpv` rather than depend on `nvim-rs`, to avoid linking LGPL code into the Harness.

## Consequences

`nvim_input` overtakes every other kind of message, which leads to two rules: nothing from the front end is sent until nvim first reports state, and submitting travels as keys so that it lands after the keys before it. A resize can be overtaken by a key sent right after it, which is harmless at human speed.

Each Session runs its own nvim, so a crash stays in one Session, but registers are shared only through the system clipboard. Quitting nvim on purpose replaces it, keeping the text but not undo history. Any other exit hands the Draft to the plain prompt. Window-opening plugins run but stay invisible, because no UI is attached. A config can check `g:sstack` to skip them.

Line numbers come from the person's `number` and `relativenumber` options: nvim sends a label for each line, and its own number column sets their width, so both sides wrap at the same column. Other things nvim draws, such as signs and folds, stay off in the prompt.

Global keys use Alt, the one modifier vim leaves free, and Alt-Enter submits in any mode. Enter also submits in Normal mode, where moving down a line is rarely wanted in a prompt, and keeps its vim meaning everywhere else. Enter is a buffer-local mapping inside nvim rather than a check in the front end, because the mode the front end last heard about can be out of date: `<Esc>` followed quickly by Enter must submit. Being buffer-local, it overrides any Normal-mode Enter mapping in the person's config. Terminals send Alt as Esc plus the key, so Esc followed by another key in the same read counts as Alt plus that key, which can happen over a slow connection.

Tests of Vim Input run a real nvim with `--clean`, behind the `nvim-tests` feature, because how nvim behaves is the main risk.
