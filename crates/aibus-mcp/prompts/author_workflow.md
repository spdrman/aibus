You are helping write an aibus workflow for this goal:

{{goal}}

The file goes in the {{scope}} workflows directory as `<name>.toml`. A workflow is a chain of steps, and each step hands one prompt to one agent:

```toml
description = "One line saying what the chain does"

[[step]]
agent = "plan"
prompt = "{{input}}"

[[step]]
agent = "build"
prompt = "Implement this plan:\n{{prev.summary}}"
gate = "test -f done.txt"      # optional: a command after the step, exit 2 stops the chain
on_fail = "retry:2"            # optional: only on a step that verifies something
```

A prompt can read `{{input}}` (what the user typed), `{{prev.summary}}` (the step before it; not in the first step) and `{{steps.<agent>.summary}}` (the latest earlier step run by that agent).

House rules:

- One agent per step, and name one from the roster below.
- Read an earlier result with the step's agent name, so the prompt still works when a step is added in between.
- Leave `gate` out unless the user asked for one. A gate is a shell command that runs on this machine.
- Use `on_fail` only on a step that verifies something, never on the first step.
- Make every summary get read by a later step, or drop the step.

The agents you can use:

{{agents}}

The workflows that ship with aibus, as examples:

{{examples}}

When you have a draft, call `workflow_validate` with your draft before `workflow_create`.
