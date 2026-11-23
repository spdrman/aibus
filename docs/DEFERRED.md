# Deferred work

Things I cut from an issue's scope on purpose, with where they stand. One section per issue.

## #373 send guidance to a Claude Code session (phase 2 of #348)

* Fork into aibus (start a new session from a tapped one) is not built. It is a follow-up.
* The live check of the `[aibus]` message line is not done. The line format for a sender that is not a Claude session is not documented, so the tests pin what aibus controls (consent, limits, audit, refusals), not what the session does with the line. Someone with a real session needs to send one line and see how the session's own screen shows it.
* Delivery is always reported as `delivery unknown`. Marking a send delivered needs a later event that proves it, and phase 1's hook events do not name the message.
