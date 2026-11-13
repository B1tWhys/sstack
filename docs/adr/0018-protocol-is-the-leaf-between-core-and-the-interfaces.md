# protocol is the leaf between core and the interfaces

Everything that passes between core and the interfaces is defined in core's `protocol` module, which imports nothing else in core. That covers the `SessionReport`s a Session sends to the interface showing it, the requests inside them, and the answers that come back: `Verdict`, the Question types and `Response`. The person's decision on a command and the model's Questions are part of the conversation between the person and the harness, so `protocol` owns them, and the tools import them from there. An interface can read the whole boundary in one module, and changing a domain module cannot change what an interface depends on by accident.

A resumed Session reaches the interfaces as a list of `Replayed` items that `agent::replay` makes from the log, not as the log's own `Entry`s. The two change at different rates. The log's shape on disk has to keep reading every older Session, while what interfaces read changes whenever core and the interfaces change together, which can be any commit. Sharing one type would have turned every change an interface wanted into a change of the log format. `Replayed` also leaves out what only the model needs, such as the Provider Payload, and core, not each interface, decides which calls a put-off Turn never started.

## Consequences

The vim types stay in `vim`, which is nvim's own channel to the interface and already imports nothing else in core.
