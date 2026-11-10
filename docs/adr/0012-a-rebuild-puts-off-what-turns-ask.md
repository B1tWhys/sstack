# A Rebuild puts off what Turns ask the person instead of waiting for it

A Rebuild waits for every Turn to reach the end of its Step, so that no Step is cut short. A Turn that has asked the person something is partway through a Step: whether a command may run, or the model's Questions. So a Rebuild used to wait for the person to answer, and a question in a Session they were not looking at could hold up the relaunch for as long as nobody noticed it. Now a Rebuild puts off every open question, of either kind, and any asked while it drains. The Turn that asked parks there, leaving the call that asked without a result. When the Turn carries on, after the relaunch or in this process if the Rebuild is backed out of, that call runs first and asks the same thing again.

The person should find each question as if the Rebuild had not happened. The log records the stop as a `TurnPutOff` entry, apart from a park between Steps, so that neither the stop nor the second asking adds anything to the transcript. The entry keeps when the person was first asked, so the Session keeps its place in the waited-longest order of the sidebar. It also keeps any Queued Prompts the Turn had not taken. They cannot be recorded as a message at the stop, because a call's result has to follow the call. The Turn takes them once that call has run, as it would have without the Rebuild.

We rejected denying the command or declining the Questions, because the model would then work around a refusal the person never gave. We rejected ending the Turn, because the person would have to repeat the request after every Rebuild, and Rebuilds happen often while the agent improves the Harness Source.

A quit still waits for every answer. Quitting a second time stops waiting, and the question is lost, as before.

## Consequences

What the person had half done in a question does not survive a relaunch, because only the Session log does: a picked choice, their own words, and which of several Questions they were on. The Questions open again on the model's recommended choices. Backing out of a Rebuild in the GUI keeps them, since the window never closed. The Draft is lost the same way, as it already was.

A Session log can now end with a Turn whose last response has calls with no result. If the person starts a new Turn instead of carrying on, those calls are recorded as not run. The results of one response can sit on either side of the stop, so the Anthropic adapter skips the stop when it groups results into one message. An answer the person gives after the question was put off does nothing, because only the first answer counts.
