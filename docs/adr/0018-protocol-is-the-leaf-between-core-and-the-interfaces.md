# protocol is the leaf between core and the interfaces

Everything that passes between core and the interfaces, in both directions, is defined in core's `protocol` module, and `protocol` imports nothing else in core. An interface author can read the whole boundary in one place, and a change to a domain module cannot change what an interface depends on by accident.

The person's answers belong to the boundary too, so `Verdict` and the Question types live in `protocol` even though the tools use them. The tools import them from there.

A resumed Session reaches the interfaces as a list of `Replayed` items, not as the log's own `Entry`s, because the two change at different rates. The log's format on disk has to keep reading every older Session. What the interfaces read can change in any commit, because core and the interfaces change together. Sharing one type would have turned every change an interface wanted into a change to the log format.

## Consequences

The vim types stay in `vim`. That module is nvim's own channel to the interface, and it already imports nothing else in core.
