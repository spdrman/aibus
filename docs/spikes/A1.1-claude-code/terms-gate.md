# Claude Code terms and distribution gate

**Gate date: 2026-11-14.** Every source below was fetched that day. The anthropic.com legal pages show an "Effective" date (given in the table); the code.claude.com docs pages show no date, so for those the fetch date is all there is. This is a record of what the documents say and where they're unclear. It isn't legal advice and I'm not drawing legal conclusions. Re-verify everything here before any release (#295), because the Usage Policy changed two days before this gate.

## Sources

| # | Source | Effective / updated | Fetched |
|---|---|---|---|
| S1 | Claude Code legal and compliance, https://code.claude.com/docs/en/legal-and-compliance | no date shown | 2026-11-14 |
| S2 | Consumer Terms of Service, https://www.anthropic.com/legal/consumer-terms | Effective Oct 8, 2025 | 2026-11-14 |
| S3 | Commercial Terms of Service, https://www.anthropic.com/legal/commercial-terms | Effective Jun 17, 2025 | 2026-11-14 |
| S4 | Usage Policy, https://www.anthropic.com/legal/aup | Effective Nov 12, 2026 | 2026-11-14 |
| S5 | Agent SDK overview, https://code.claude.com/docs/en/agent-sdk/overview | no date shown | 2026-11-14 |
| S6 | Support: log in to your Claude account, https://support.claude.com/en/articles/13189465-log-in-to-your-claude-account | "Updated over a week ago" | 2026-11-14 |
| S7 | Claude Code LICENSE.md, https://raw.githubusercontent.com/anthropics/claude-code/main/LICENSE.md | n/a | 2026-11-14 |
| S8 | Trademark Guidelines, https://www.anthropic.com/legal/trademark-guidelines | Effective Aug 1, 2024 | 2026-11-14 (summarised fetch only, not checked against the raw page) |
| S9 | Headless mode, https://code.claude.com/docs/en/headless | no date shown | 2026-11-14 |

## What they say that matters here

Which terms apply (S1): "Your use of Claude Code is subject to: Commercial Terms of Service - for Team, Enterprise, and Claude API users; Consumer Terms of Service - for Free, Pro, Max, and Ultra users". S7 says something different: "© Anthropic PBC. All rights reserved. Use is subject to Anthropic's Commercial Terms of Service." That's a mismatch between two of Anthropic's own documents.

Products that preinstall or run Claude Code (S1): "Unless we've mutually agreed otherwise, preinstalling or running Claude Code in your products or services (e.g. in hosted sandboxes or other agent infrastructure) requires agreeing to our Commercial Terms of Service and complying with the conditions below". The conditions: "The Claude Code binary must not be modified", nobody may "remove, disable, or restrict any authentication method built into it", and "Customers may not pay for, resell, or intermediate Claude usage on their end users' behalf. Each end user must authenticate with their own Anthropic API key, Claude subscription plan credentials, or 3P inference provider credential".

End users on their own subscription (S1): the restrictions don't prevent "an end user from signing in to the unmodified Claude Code binary with their own Claude subscription, including where a platform hosts Claude Code as described under *Can customers offer Claude Code in their products?* above."

Developers and credentials (S1): "Developers building products or services that interact with Claude's capabilities, including those using the Agent SDK, should use API key authentication through Claude Console or a supported cloud provider. Anthropic does not permit third-party developers to offer Claude.ai login into their own applications, or to route requests through Free, Pro, Max, or Ultra plan credentials on behalf of their users." Also: developers "may not collect, store, or intermediate Claude.ai credentials or session tokens", and sign-in "must complete through Anthropic's own flow".

Usage limits (S1): "Advertised usage limits for Pro, Max, and Ultra plans assume ordinary, individual usage of Claude Code and the Agent SDK."

Agent SDK (S5): "Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK." The same page describes the integration aibus uses: "run the CLI as a subprocess with the `-p` flag and `--output-format json`", and S9 calls `claude -p` "using the Agent SDK via the CLI".

Third-party tools (S6): "The preferred way to access Anthropic services using third-party software, tools, or services ("third-party tools"), including open-source projects, is through API key authentication through Claude Console or a supported cloud provider. Anthropic may at its discretion allow paid subscribers who have enabled usage credits to use certain third-party tools to access Anthropic services included in paid subscription plans, but reserves the right to draw use of such third-party tools from usage credits rather than subscription limits." And: "Use of third-party tools that misrepresent their identity to Anthropic's servers, attempt to route third-party traffic against subscription limits, or otherwise violate applicable terms or policies is prohibited".

Automation in the Consumer Terms (S2, section 3, prohibited): "Except when you are accessing our Services via an Anthropic API Key or where we otherwise explicitly permit it, to access the Services through automated or non-human means, whether through a bot, script, or otherwise."

Usage Policy (S4): no one may "Resell, proxy, or otherwise provide access to Claude through unauthorized means, including services that route requests through consumer subscriptions or misrepresent the product or client being used".

Names (S1): "You can accurately say, in plain text, that your product has Claude Code preinstalled or that it runs Claude Code. But you can't use the Claude Code or Anthropic names or logos as part of your own product, feature, or company name". S5 also lists "Claude Code" and "Claude Code Agent" as not permitted product names.

## Gate

| Use | Status on 2026-11-14 | Why |
|---|---|---|
| **Open-source, local, user's own unmodified `claude`, signed in with an API key / Console** | **Proceed (pool builds toward this)** | Matches S1/S5/S6's developer path exactly. aibus never sees the key; the CLI owns it |
| **Same, signed in with a claude.ai subscription (Pro/Max/Ultra)** | **Ambiguous: owner decision before aibus documents or advertises it** | S1 allows an end user to sign the unmodified binary in with their own subscription, even on a hosting platform. But S5 says third-party developers may not "offer claude.ai login or rate limits for their products" unless approved, S6 prefers API keys for third-party tools "including open-source projects" and may charge such use to usage credits, and S2 bars scripted access unless "explicitly permit[ted]". Whether a local tool that launches the user's own install is "offering" claude.ai login is exactly what the documents don't say |
| aibus downloads, bundles or preinstalls Claude Code | **Blocked** | S1 requires the Commercial Terms plus the conditions above; S7 grants no redistribution right ("All rights reserved") |
| Hosted aibus running Claude Code for users | **Blocked** | S1 names "hosted sandboxes or other agent infrastructure" as needing the Commercial Terms plus conditions |
| Commercial / paid release | **Blocked pending review** | #280 decision 6 and spec 10.1. Nothing here clears it |
| Extension or package named with "Claude Code" | **Change needed: owner picks a name** | S1 and S5 rule out the name as part of a product or feature name; plain-text "runs Claude Code" is fine |

I didn't find anything that rules out the technical approach itself: an unmodified binary, the user's own sign-in through Anthropic's flow, no token handling, documented flags only. Those are the conditions S1 sets for the products it does allow, and the spike kept to all of them.

## What aibus has to keep doing for this gate to hold

- Launch only the user's own installed, unmodified binary. Never download, bundle, patch or wrap it.
- Never read, copy, proxy or store a credential, and never run a login flow other than handing the terminal to `claude auth login` itself.
- Never pool: one user, one sign-in, their own billing.
- Never strip or hide any of the CLI's sign-in options, and never misrepresent the client (no user-agent or identity tricks).
- Show `authMethod` from `claude auth status` so the user knows which terms and limits they're under.
- Keep "Claude Code" out of product, feature and package names.

## Approval register entry

| Provider | Official CLI | Version checked | Auth | Open-source local | Bundled | Hosted | Commercial | Reviewed | Next review |
|---|---|---|---|---|---|---|---|---|---|
| Anthropic | Claude Code (`claude`, Developer ID Anthropic PBC Q6L2SF6YDW) | 2.1.311 | Vendor-owned (`auth login` / `auth status`) | API key: proceed. Subscription: owner decision | Blocked | Blocked | Blocked | 2026-11-14, #281 | Before any release (#295), or when S1, S2, S4, S5 or S6 change |

## Owner questions

1. Subscription sign-in for local open-source use: ask Anthropic (S1 says to contact sales about authentication methods), accept the ambiguity, or tell users to sign Claude Code in with an API key when they use it through aibus?
2. A new extension id and package name without "Claude Code" in it.
