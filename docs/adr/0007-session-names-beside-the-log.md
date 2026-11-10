# Session names live beside the Session log

The name a person gives a Session with `/rename` is kept in a file next to its log, `<id>.name`, not as an entry in the log. ADR 0002 makes the log the one record of a Session, so the obvious place would be a rename entry. We rejected that because the log records the conversation the model sees, and a name is not part of it. Only the agent's task writes the log, so a rename typed during a Turn would have to queue behind that Turn. A rename entry landing between tool results would also split them into separate messages for the Provider. And `is_parked` reads the last entry, so a rename after a parked Turn would hide the Turn from being carried on. A separate file avoids all three, at the cost that archiving and restoring a Session must move two files instead of one.

A Session Name made by summing up the first prompt is kept in the same file. It is written only if the Session has no name yet, so a `/rename` made while the summary is on its way wins.

## Consequences

A name was first kept alone in `<id>.name`. It now lives in the Session's Session Metadata, one JSON file `<id>.json` beside the log, so that later facts about a Session join the same file instead of each adding one. The same reasons keep them all out of the log. Facts the Running Harness does not know are kept as they are when it rewrites the file, because a Rebuild can relaunch an older build of the Harness Source.
