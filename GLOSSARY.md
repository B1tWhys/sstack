# sstack

A native agent harness that starts minimal and is improved by using it to work on itself.

## Language

### Conversation

**Session**:
One durable conversation between a person and the agent, recorded as an append-only log.
_Avoid_: Chat, thread, conversation

**Session Name**:
What a person knows a Session by: the name they gave it with `/rename`, or else a short summary of the first thing they said in it.
_Avoid_: Title, label

**Session Metadata**:
What is kept about a Session beside its log because it is not part of the conversation the model sees, such as its Session Name.
_Avoid_: Session info, sidecar

**Turn**:
One drain of admitted input in a Session, ending when the model and its tools have stopped. A Turn contains zero or more Steps.
_Avoid_: Round, exchange

**Step**:
One model request plus the tool executions caused by its response.
_Avoid_: Iteration, call

**Question**:
Something the model asks the person during a Turn, offering answers to choose from and room for their own, which holds up the Turn until they answer.
_Avoid_: Prompt, query, survey

### Models

**Provider**:
A service that serves models through an API. Sessions are never tied to one Provider.
_Avoid_: Backend, vendor

**Connection**:
A Provider paired with one set of credentials and the models to request from it. Each Session runs on one Connection at a time, which the person can change between Turns. Several Connections can use the same Provider.
_Avoid_: Account, profile

**Provider Payload**:
The exact data a Provider returned for a model response, kept unchanged inside that response's Session entry and only meaningful to that Provider.
_Avoid_: Raw blocks, vendor data

### Self-improvement

**Running Harness**:
The program currently executing a Session.
_Avoid_: The harness (when the Harness Source could be meant)

**Harness Source**:
The version-controlled code from which a Running Harness is built, and which the agent edits to improve it.
_Avoid_: The harness (when the Running Harness could be meant)

**Seed**:
The smallest Harness able to make a real change to its own Harness Source with no outside tool.
_Avoid_: MVP, prototype

### Interfaces

**Interactive Mode**:
Running the Harness in a terminal so that a person works through the TUI, as opposed to Headless Mode.
_Avoid_: TUI mode

**Headless Mode**:
Running the Harness on one prompt given when it starts, with no interface and no person to answer questions.
_Avoid_: Batch mode, non-interactive mode

**TUI**:
The full-screen interface a person uses in a terminal.
_Avoid_: Terminal UI

**GUI**:
The windowed interface a person uses on the desktop.
_Avoid_: Desktop app, window

### Tools

**Workspace**:
The directory the model works in during a Session: the main checkout, or the Session's own worktree.
_Avoid_: Project root, workplace, working copy

**Protected Path**:
A path in the Workspace that the model can never see or change by any tool, such as a `.git` directory or a `.env` file.
_Avoid_: Hidden file, secret file

**Sandbox**:
The container a Session's shell commands run in without asking the person. It sees the Workspace, without its Protected Paths, and the crates the person has downloaded, which it cannot change. It has no network.
_Avoid_: Jail, container (when the Sandbox is meant)

### Input

**Draft**:
Text a person has typed but not submitted, along with where its cursor is.
_Avoid_: Buffer, pending input

**Queued Prompt**:
A prompt the person sent while a Turn was running, which waits for the agent to take it at the end of the current Step, or to start the next Turn on it if that Turn ends first.
_Avoid_: Pending message, interjection, steering

**Vim Input**:
Typing into a real nvim that runs behind the Harness, as opposed to the plain prompt. The nvim does all the editing, and the Harness only passes keys in and draws what comes back.
_Avoid_: Vim mode, vim emulation

### Commands

**Human Command**:
A slash-prefixed instruction a person gives the Harness directly, which never reaches the model.
_Avoid_: Skill, slash command

**Skill**:
A directory of instructions for the model, described by its `SKILL.md`, that a project or a person keeps in an `.agents/skills` directory shared with other agent tools. A person can run one by typing `/` and its name.
_Avoid_: Plugin, Human Command

**Skill Scope**:
Where a Skill was found: the project, a directory nested inside it, or the person's own Skills. A Skill's name shows its Scope, as in `personal:writing-prose`, only when another Skill shares the name.
_Avoid_: Source, level, workspace

**Rebuild**:
The Human Command that builds the Harness Source, from the main checkout or from the Session's worktree, and only if that succeeds relaunches into the same Session.
_Avoid_: Restart, reload
