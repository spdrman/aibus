# Deferred

## A3.1 extensions (#291), cut on 2026-11-22

- Install, login-state and launch tests against a real Kimi Code, Copilot CLI, Cursor Agent and
  Qwen Code, then replacing the synthetic fixtures with recordings and tightening `cli_versions`.
- Tool failure and error-result mappings, which need each CLI's real failure events.
- A published community fixture pack. The fixtures sit under `examples/extensions/<name>/fixtures`
  for now, and where to publish them (repo, the A2.1 catalog or a release asset) is still open.
- Building anything for xAI/Grok or DeepSeek: xAI has a first-party CLI (Grok Build) but stays `disabled` until its terms are read, DeepSeek is `unavailable`.
