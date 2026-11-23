# Provider feasibility register

A dated yes or no on whether a first-party agent CLI exists that aibus may launch (an official,
unmodified executable, a documented automation surface, and a permitted way to use it). A "no"
means the provider stays `unavailable` and there is no fallback to an undocumented endpoint. A
"yes" is only a yes for that date, and it is not a build decision on its own...

I did this one from what I already knew and the repo, not from a fresh read of each vendor's
terms, so the "no" rows say what I could not confirm on the date, not that no such CLI can exist.
Whoever owns the step re-checks the vendor pages below before anything gets built.

| Provider | First-party agent CLI with an automation surface | Checked | Evidence | Status |
|---|---|---|---|---|
| xAI/Grok | no | 2026-11-22 | I could not confirm an official, documented xAI agent CLI with a headless mode and embedding terms. The repo only has the `xai` API preset (`catalog/xai.json`), which is a direct API path. | `unavailable` |
| DeepSeek | no | 2026-11-22 | I could not confirm a first-party DeepSeek agent CLI. The repo only has the `deepseek` API preset. DeepSeek models stay reachable through separately billed OpenRouter. | `unavailable` |

Unverified: both rows rest on the date above and on no live check of the vendors' sites. The owner
step is to open xAI's and DeepSeek's developer docs, look for an official CLI release, its
automation flags, its login semantics and its redistribution and trademark terms, and then change
the row. No unofficial client gets built in the meantime.
