# A Session keeps its own Connection

The person picks a Connection for each Session, and can change it between Turns from the TUI or the GUI. Before, the Connection was chosen once per run with `--connection`, and every Session of the run used it. ADR 0002 already lets a Session move between Providers, so the choice belongs to the Session, not the run. A switch requested while a Turn runs waits until that Turn ends, so that no Turn mixes two models. The Session keeps the name of its Connection in its Session Metadata (ADR 0007), so it resumes on the same Connection, after a Rebuild too.

Each Connection names its own `model` and `name_model`, and the global `default_model` and `name_model` only fill in for one that leaves them out. Picking a Connection then also picks models its Provider serves. We rejected a separate model picker, because it would let the person pair a Claude model with an OpenAI secret.

We rejected switching every open Session at once, which is what relaunching with another `--connection` amounted to, because Sessions run side by side on unrelated work. We rejected a switch that applies only to Sessions started afterwards, because the person often wants a second opinion on a Session that already has history.

## Consequences

`--connection` applies to the first Session a run opens and to Sessions started during the run, each of which keeps it. A stored Session opened later stays on its own. A Rebuild no longer passes `--connection` on, since the Session remembers. `--model` still overrides the model of each Session the run opens, but a switch uses the new Connection's own model.

A switch looks up the new Connection's secret before it happens, and refuses with a notice if the secret is missing, so a Session never ends up on a Connection that cannot run. Commands on the host are kept from the secrets of every Connection in settings, not only the current one, because the Session can switch to any of them.

A switch keeps the system prompt and tools, which were built when the Session opened. The Skill listing in the system prompt was sized for the first Connection's context window, so it can be too long or too short for the new one until the Session is opened again. Moving to another Provider drops the old reasoning from requests and misses the prompt cache once, for the reasons in ADR 0003. Nothing stops a switch to a Connection whose context window is smaller than the Session has grown, and the next request then fails as a Turn error.
