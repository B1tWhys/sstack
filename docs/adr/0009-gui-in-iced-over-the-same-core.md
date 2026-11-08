# A GUI in iced over the same core as the TUI

sstack gets a GUI, built with iced, as a second interface beside the TUI. iced won because its model of messages into `update` and state out through `view` matches the event-and-action design of ADR-0004, and `iced_test` lets tests drive the widgets headlessly, so the agent can change the GUI without seeing it. `pane_grid` gives resizable splits, and `iced::daemon` gives several windows. Tabs, trees and a transcript that lays out only what is visible are widgets we will write ourselves. We rejected egui because it fits the component design less well, although `egui_dock` has more ready-made docking. We rejected Tauri because it adds a second language, a JavaScript toolchain and a second test stack, and a Rebuild would have to build both halves. We rejected gpui because its API is unstable and thinly documented outside Zed.

The GUI is its own binary, `sstack-gui`, in its own crate, and a Rebuild builds and relaunches whichever binary is running. One binary with a `--gui` flag would make every TUI build link iced.

What a Session is doing, and what the Running Harness is doing during a quit or a Rebuild, are domain, so they live in core with the Turn runner, Human Commands, Rebuild and the setup of a Session. Each interface is a thin view: it turns what the person did into calls on core and draws what core reports. We first considered a third crate for this shared code and dropped it, because nearly everything that would have gone in it is already a term in the glossary.

Two kinds of state stay in an interface on purpose. Transient state that only one window cares about, such as a highlighted button, a scroll position or which panes are open, may live in the GUI when that is simpler. Each interface also builds its own transcript from the Session's reports. The TUI and the GUI will present a transcript differently, so the 30 or so lines that join streamed text are not duplicate code, even where they match.

## Consequences

The first GUI opens one Session per window, and is started from a terminal inside the project, taking the TUI's flags except `--prompt`. Choosing a project, an app bundle and a list of Sessions come later. Its styling stays minimal until a session dedicated to the visual style.

A Running Harness holds an exclusive lock on its Session's log, so the TUI and the GUI cannot both write to one Session. The kernel releases the lock when the process exits, however it exits.
