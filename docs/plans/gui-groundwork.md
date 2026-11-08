# Plan: groundwork for the GUI

The goal is a GUI good enough that the person can work on sstack in it, in place of the TUI. The design is settled in `docs/adr/0009-gui-in-iced-over-the-same-core.md`; read it, ADR-0004, `GLOSSARY.md` and `AGENTS.md` before starting a step.

## How to run a step

Each step is done by one agent with a fresh context, in order, because each step builds on the one before it. Do only your step.

1. Read this file, the documents above, and the code your step names.
2. Make the change test-first where the step adds behaviour. Steps 2 to 5 are refactors: the TUI and headless mode must behave exactly as before, and every existing test and snapshot must pass unchanged.
3. Before merging, extracting or copying any code, load the `judging-duplicate-code` skill and apply its test.
4. Run `cargo nextest run --workspace --all-features` and commit with a conventional commit message. The hooks must pass, which includes every function's CRAP score being 6 or less. Never bypass them and never push.
5. Tick your step below and write a short note under it: anything the next step needs to know, and anything you found that this plan got wrong.

A step is done when its commit is in, the full test run is green, and its note is written. If the step cannot be done as written, stop, leave the code as it was, and explain why in the note.

## Steps

### [x] 1. Lock the Session log

A Running Harness holds an exclusive lock on a Session's log for as long as it has the Session open, so that two Harnesses can never append to the same log. Take the lock with `File::try_lock` in `SessionLog::create` and `SessionLog::open` (`crates/core/src/session.rs`), and add a `SessionError` variant whose message is "this Session is open elsewhere". Reads that only look at a log, such as `first_prompt` and `is_parked`, take no lock. Check that the TUI and headless mode show the error well. Test that opening a log a second time in the same process fails.

Note: The variant is `SessionError::OpenElsewhere`. I added `session::lock_free_read(path)`, a lock-free read of everything recorded so far, because many tests read a log back while an Agent still had it open. Use it, not `SessionLog::open`, wherever code only looks at a log. The lock lasts as long as the `SessionLog`. In the TUI that means as long as the worker task holding the `Agent`, which can outlive the `App` by a few polls. The test harness in `tui/app.rs` therefore has a `stopped()` helper that waits for the event channel to close before a test relaunches into the same Session. A real Rebuild needs nothing extra, because Rust opens files close-on-exec and the `exec` in `rebuild.rs` releases the lock. I checked the messages by reading the code rather than running a second Harness. At startup, headless mode and the TUI print `error: this Session is open elsewhere` before the TUI takes the terminal. Inside the TUI, the person sees `could not open the Session: this Session is open elsewhere` when switching to a Session that is open elsewhere, or `could not carry on a Session: ...` for a parked one. A GUI and a TUI on the same project can now list each other's Sessions but not open them.

### [x] 2. Move the lifecycle, Human Commands and Rebuild into core

Move `crates/cli/src/tui/lifecycle.rs` (`SessionState`, `SessionEvent`, `Phase`, `Trigger`, `Effect`, `Exit`, `interrupt`), `crates/cli/src/command.rs` and `crates/cli/src/rebuild.rs` into the `sstack` crate. Rebuild currently hardcodes `--package sstack-cli` and the binary `sstack`. Make it build and relaunch the package and binary that is running, given by the caller.

Note: The new modules are `sstack::human_command` (`Command`, `parse_command`), `sstack::running_harness` (`SessionState`, `SessionEvent`, `Phase`, `Trigger`, `Effect`, `Exit`, `Interrupt`, `interrupt`) and `sstack::rebuild` (`Binary`, `build`, `relaunch`). `rebuild::build` takes a `rebuild::Binary`, which gives the cargo package and the binary's name. The CLI passes its own as the `HARNESS` const in `crates/cli/src/lib.rs`: package `env!("CARGO_PKG_NAME")`, binary `sstack`. The GUI should define its own the same way, with `sstack-gui` for both. Rebuild still finds the Harness Source from core's own `CARGO_MANIFEST_DIR`, and the profile from `cfg!(debug_assertions)` in core. Both match the binary because every crate sits in one workspace and is built with one profile. `Phase::progress` did not move: it is now the function `progress` in `tui/app.rs`, because its text names Alt-C. For the same reason, the docs in `running_harness` speak of an interrupt and an interface, not Alt-C and the `App`. `Command` still has variants only the TUI acts on (`New`, `NewMain`, `Archive`, `Vim`, `Rename`), so step 5's controller has to pass those back to the interface. `relaunch_args`, which gives the arguments for the relaunched binary, is still in `setup.rs` and moves with step 4.

### [x] 3. Split the Session reports out of the TUI's events, and move the Turn runner

`tui::Event` (`crates/cli/src/tui/event.rs`) mixes terminal input (`Key`, `Paste`, `Resize`, `Scroll`, `Click`, `Vim`) with reports about a Session (`Resumed`, `TurnStarted`, `TurnContinued`, `Agent`, `TurnEnded`, `Approval`, `BuildFinished`, `Notice`). Move the reports into core as their own type, with `ApprovalRequest` and `Reply`. The TUI keeps its `Event` and wraps the core type. Then move `AgentHandle` (`crates/cli/src/tui/worker.rs`) into core, reporting through the new type.

Note: The reports are `sstack::session_report::SessionReport`, with the same eight variants the TUI had. `ApprovalRequest` and `Reply` sit beside it. Core reports through a `ReportSender`, which is a cloneable wrapper around a function `Fn(SessionReport) + Send + Sync` that the interface chooses when it creates one with `ReportSender::new`. The function must not block, because core calls it from inside a Turn's event callback. The TUI's `Event` has one `Event::Report(SessionReport)` variant in place of the eight. `SessionSender::reporter` returns a `ReportSender` whose reports arrive as `Event::Session(id, Event::Report(..))`, on the same channel as before, so ordering is unchanged. The GUI can make its own with a closure that sends into a channel its `Subscription` reads, and can tag reports with a window or Session id there. `AgentHandle` is now `sstack::turn_runner::AgentHandle`, and `AgentHandle::spawn` takes the agent and a `ReportSender`. I also moved the TUI's `EventApprover`, the Approver that sends an `ApprovalRequest` and waits for the `Reply`, into core as `session_report::ReportingApprover`, because the GUI asks about shell commands in exactly the same way. For step 4, `prepare` still takes a closure that makes an Approver for a Session id, and the TUI builds `ReportingApprover::new(SessionSender::new(id, ..).reporter())`. The core setup can take a `ReportSender` per Session instead. `BuildFinished` and `Notice` are not about any one Session's Turns. The TUI sends `BuildFinished` straight on its event channel, not wrapped in `Event::Session`, and only shows `Notice` on a Pane by calling `update` directly; core sends neither yet. Step 5's controller will be the first core code to produce them.

### [x] 4. Move the setup of a Session into core

Move `crates/cli/src/setup.rs` into core, except for anything that reads the clap `Args`, which stays in the binaries. Core takes plain values: which Session to start or resume, the Connection and model if overridden, and where the Session works. `describe.rs` holds the words headless mode and the TUI show for what happens in a Session. It is presentation, so it stays in the CLI crate.

Note: The module is `sstack::session_setup`. Its entry point is the `bon` builder `prepare()`, which takes `start` (a `Start`, with `Place`), `directory` (the directory the person started in), the optional `connection` and `model` (set them with `maybe_connection` and `maybe_model` from an `Option<&str>`), `approver` (a closure that makes an Approver from the Session id) and `containers`. It returns a `Run` or a `SetupError`, whose messages match the old anyhow ones. `main_checkout`, `sessions_directory` and `installed_skills` take the start directory too, so core no longer reads the current directory. `LATEST` moved into core, and the CLI's `--resume` default names it from there. A binary must still supply four things itself: the start directory (both binaries pass `std::env::current_dir()`), the approver, the shared `Containers`, and the words for `Run::descriptions_left_out`. That field is the count of Skills listed without their descriptions. The TUI turns it into a notice with `describe::listing_shortened` and appends it after `Run::notices`, in the same order as before. The GUI should show a similar warning in words of its own. `relaunch_args` stayed in the CLI, because it writes out the CLI's own flags. It is now `Args::relaunch` in `crates/cli/src/lib.rs`, with its test. Step 6 decides, through `judging-duplicate-code`, whether the GUI shares the flag definitions and this method or writes its own. The `dirs` dependency moved from the CLI crate to core.

### [ ] 5. Pull a controller for one Session out of the TUI's App

`App` (`crates/cli/src/tui/app.rs`) mixes the screen with logic that every interface needs. Move that logic into core as a controller for one Session. It covers `submit`, which decides whether a line is a Human Command, a Skill or a prompt, `advance`, `check_settled`, `interrupt`, `resume_parked`, `build_finished` and the Phase they drive. The interface tells the controller what the person did and what core reported, and carries out what the controller asks for. Everything about several Sessions at once stays in the TUI: the sidebar, `queue`, `step`, `show_longest_waiting`, `open_new` and archiving. So do `/rename` and `/vim`. Load the `codebase-design` skill before choosing the controller's interface. If the seam is unclear, stop and describe the options in the note.

### [ ] 6. Add the GUI crate

Add `crates/gui`, package `sstack-gui`, with a binary of the same name, built on the latest iced release. It takes the TUI's `--resume`, `--connection` and `--model` flags, and works on the project in the directory it is started from. Apply `judging-duplicate-code` before sharing the flag definitions with the CLI. It opens one Session in one window, with a plain multi-line prompt that runs a Turn through the controller, and shows the reply as plain text. Session reports reach the iced app through a `Subscription`. Keep styling to iced's defaults: the visual style gets a session of its own.

Test the logic by feeding `Message`s to `update` and checking state. Add a few `iced_test` tests that drive the real widgets and check structure and text. Add no pixel snapshots yet.

### [ ] 7. Make the GUI usable for work on sstack

Commit each item separately:

- A transcript that streams the reply as it arrives, shows the model's reasoning, and shows each tool call as a one-line summary. The GUI builds its own transcript from the Session reports; ADR-0009 explains why it does not share the TUI's.
- Approval of a shell command, for when the Sandbox is unavailable.
- Interrupting a running Turn.
- Rebuild, by `/rebuild` and by a button, which relaunches `sstack-gui` into the same Session.

After this step the person can work on sstack in the GUI.
