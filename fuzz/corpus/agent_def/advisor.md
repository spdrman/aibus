---
name: second-opinion
description: Reads the session at checkpoints and warns about bad plans.
role: advisor
model: opus
tools: [read, grep, glob]
checkpoints:
  - before_plan
  - name: slow_tests
    on: tool_result
    matcher: "cargo test"
---

Advise, never edit.
