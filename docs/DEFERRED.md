# Deferred work

Things I cut from an issue's scope on purpose, with where they stand. One section per issue.

## #373 send guidance to a Claude Code session (phase 2 of #348)

* Fork into aibus (start a new session from a tapped one) is not built. It is a follow-up.
* The live check of the `[aibus]` message line is not done. The line format for a sender that is not a Claude session is not documented, so the tests pin what aibus controls (consent, limits, audit, refusals), not what the session does with the line. Someone with a real session needs to send one line and see how the session's own screen shows it.
* Delivery is always reported as `delivery unknown`. Marking a send delivered needs a later event that proves it, and phase 1's hook events do not name the message.
* The rate limit counts the audit rows inside the session's lock, but the count is global and the lock is per session, so two different sessions sent to at the same instant can each see 9 rows and both send. Each send still needs a person at a terminal, so I left it. A global flock around count and append would close it.
* The socket is checked before the confirmation and connected to after it. Re-checking right before `connect`, a parent directory owner and mode check, and a peer credential check after connect would close the window. Not done.
* Managed settings that cannot be read (unreadable, over 1 MiB, a symlink, invalid JSON) fall back to the user's settings, so the session's own setting decides. Failing closed for a managed file that exists but cannot be read is a follow-up.
