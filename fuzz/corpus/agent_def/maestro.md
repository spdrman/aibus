---
name: maestro
description: Session lead that plans, delegates and applies advice from the advisor.
role: lead
model: claude-sonnet-4-5
account: anthropic-work
effort: high
permission_mode: accept_edits
max_turns: 80
max_tokens_total: 2000000
max_cost_usd: 12.5
skills: [rust, review, rust]
memory: project
background: false
initial_prompt: Read AIBUS.md and tell me what is open.
auto_compact_at: 150000
disallowed_tools:
  - bash(rm *)
hooks:
  pre_tool_use:
    - matcher: bash
      command: ./scripts/readonly-guard.sh
      timeout_secs: 5
---

You lead the session. Delegate searches to workers, keep the user informed, apply the advisor's
advice on the next turn.
