# A Rebuild puts off questions about commands instead of waiting for them

A Rebuild waits for every Turn to reach the end of its Step, so that no Step is cut short. A Turn that has asked the person whether a command may run is partway through a Step, so a Rebuild used to wait for the person to answer. A question in a Session the person was not looking at could hold up the relaunch for as long as nobody noticed it. Now a Rebuild puts off every open question about a command, and any such question asked while it drains. The Turn that asked parks before running the command, and the call that asked is left without a result. When the Turn carries on, after the relaunch or in this process if the Rebuild is backed out of, that call runs first and asks the same question again.

We rejected denying the command, because the model would then work around a refusal the person never gave. We rejected ending the Turn, because the person would have to repeat the request after every Rebuild, and Rebuilds happen often while the agent improves the Harness Source.

This covers only questions about commands. A Rebuild still waits for the person to answer the model's Questions. A quit still waits for every answer. Quitting a second time stops waiting, and the question is lost, as before.

## Consequences

A Session log can now end with a parked Turn whose last response has calls with no result. If the person starts a new Turn instead of carrying on, those calls are recorded as not run. The results of one response can sit on either side of the park, so the Anthropic adapter skips the park when it groups results into one message. An answer the person gives after the question was put off does nothing, because only the first answer counts.
