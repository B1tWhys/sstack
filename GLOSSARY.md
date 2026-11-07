# sstack

A native agent harness that starts minimal and is improved by using it to work on itself.

## Language

### Conversation

**Session**:
One durable conversation between a person and the agent, recorded as an append-only log.
_Avoid_: Chat, thread, conversation

**Turn**:
One drain of admitted input in a Session, ending when the model and its tools have stopped. A Turn contains zero or more Steps.
_Avoid_: Round, exchange

**Step**:
One model request plus the tool executions caused by its response.
_Avoid_: Iteration, call

### Models

**Provider**:
A service that serves models through an API. Sessions are never tied to one Provider.
_Avoid_: Backend, vendor

**Connection**:
A Provider paired with one set of credentials, selected at runtime. Several Connections can use the same Provider.
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

### Input

**Draft**:
Text a person has typed but not submitted, along with where its cursor is.
_Avoid_: Buffer, pending input

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
The Human Command that builds the Harness Source and, only if that succeeds, relaunches into the same Session.
_Avoid_: Restart, reload
