---
name: code-reviewer
description: Reviews a diff for correctness and security.
tools: Read, Grep, Glob, Bash
disallowedTools: Write, Edit
model: inherit
omitClaudeMd: true
---

Run git diff and report by priority.
