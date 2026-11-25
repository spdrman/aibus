# Moving a 0.1.0 config to 0.2.0

Every 0.1.0 config loads unchanged in 0.2.0. I kept `fixtures/config/` (one file per provider id from 0.1.0) in the test run for exactly that, and nothing below is something you have to do before your next session. This page is for when you want the new forms, and for the three places where 0.2.0 reads a 0.1.0 file differently from how it used to.

The short version of what changed: a provider is now a definition (a preset, or a `[[provider]]` table you write) that names a wire protocol, so a vendor that speaks OpenAI's or Anthropic's or Gemini's protocol is a few lines of config and no code. The full picture is in `docs/PROVIDERS.md`.

## The three cases that change shape

### `openai-compatible` with a `base_url`

In 0.1.0 a local or third-party server was an account on the `openai-compatible` provider with the URL on the account:

```toml
[[account]]
name = "home"
provider = "openai-compatible"
base_url = "http://10.0.0.5:11434/v1"
auth = { none = true }
```

That still works. What 0.2.0 prefers is a `[[provider]]` of its own, because the URL then belongs to a provider id and two accounts can share it:

```toml
[[provider]]
id = "home"
api = "openai-chat"
base_url = "http://10.0.0.5:11434/v1"

[[account]]
name = "home"
provider = "home"
auth = { none = true }
```

`aibus doctor` prints the exact `[[provider]]` for any `openai-compatible` account that has a `base_url`. A few servers have a preset now (`ollama`, `lm-studio`, `vllm`, `llama-cpp`, `litellm`, `groq`, `openrouter` and others, listed in `docs/CAPABILITIES.md`), and for those you only write the account. Two accounts of one provider id that set different `base_url`s in the old form are still an error, since one provider instance cannot serve both; the fix is two `[[provider]]` ids.

### `api = "responses"`

In 0.1.0 the OpenAI Responses API was a field on the account:

```toml
[[account]]
name = "openai-work"
provider = "openai"
api = "responses"
```

In 0.2.0 that is the same as a provider whose `api` is `openai-responses`, and the account line still works. If you want it written the new way, add `[[provider]] id = "openai"` with `api = "openai-responses"` (a `[[provider]]` with the id of a preset overrides that preset field by field).

### `models.json` files that gain capability fields

Your `~/.config/aibus/providers/<provider>/models.json` from 0.1.0 has sizes, a family, a deprecated flag and maybe prices. It still loads, and it keeps overriding the built-in table. What is new is that a model entry can also say what the model can do:

```json
{ "id": "my-model", "context_window": 32768, "max_output": 4096, "family": "mine", "deprecated": false,
  "reasoning": true, "capabilities": { "tools": true, "images": false, "logprobs": false } }
```

Every capability field is optional and the layers merge field by field, so an old file that only fixes a context window keeps the built-in table's claims about tools and images instead of wiping them. The older `"effort": true` on an entry still means `capabilities.effort`. If you wrote a file to switch a feature off for a model, the new place to say it is `capabilities`.

## Things that are only new

* `[[account]] fallback = ["other-account"]` and `[defaults] fallback_on = [...]` move a run to the next account when one runs out of quota or is overloaded before the first token. Off unless you write it.
* More `auth` forms: `oauth = true` (where a vendor publishes a login for third-party apps), `sigv4`, `aws_env` and `aws_profile` (Bedrock), `gcloud` (Vertex). The old forms are untouched.
* More providers: `gemini` (the native API), `bedrock` and `vertex` (Claude on those clouds), and the presets in `docs/CAPABILITIES.md`.
* The built-in model tables moved to `catalog/` at the repo root. Nothing about `models()` or prices changes, and I fetch no third-party catalog at run time.
* `aibus doctor` prints a row per provider and warns about a `[[provider]]` no account uses. A `base_url` that is plain `http://` to an address that is not this machine or a private network gets a warning when the config loads.

# From 0.2.x to the EPIC A model (assumption, #295)

I am assuming "migration guidance from v2" in #295 means someone on aibus 0.2.x (0.2.0 or 0.2.1) moving to what EPIC A added. The issue does not say, and if it meant the v2.0 EPIC A spec instead, this is the wrong document and a spec-delta note is still owed.

Nothing here has to be done. I did not remove or rename a config key, a command or a file format in Phase 2 or 3 that I know of, and the 0.1.0 fixtures in `fixtures/config/` still load in the test run. I did not run a real 0.2.1 user directory through this build for this page, so treat "nothing breaks" as what the tests say and not as a promise. What is new, and off until you use it:

* **Workflows** keep the v1 file format. The compiler adds a plan with a stable hash (`aibus workflow plan --dry-run`) and runtimes beyond `aibus` (`cli.codex`, `cli.gemini`, `openrouter`) that plan but do not run yet, because the scheduler has no host (D-283, D-359, docs/DEFERRED.md). A workflow you already have runs on the chain runner as before.
* **Extensions**: `aibus extensions ...` manages local, signed or unsigned packages from a registry directory you point at. Nothing is installed by default, and nothing is fetched over a network. The program an extension may start has to be on the host's list (`claude`, `codex`, `gemini`, `kimi` and names you add to `extension-policy.toml`). See docs/spec/16-extensions.md.
* **Runs and traces**: `aibus runs compare`, `runs repro` and `runs inspect --node --json` read recorded traces. They never start a run.
* **OTLP export** has no config key. It does not run, and the `OTEL_*` variables are ignored on purpose (docs/OTEL.md).
* **Handoffs between providers** need a recorded consent per transfer once the scheduler runs them (D-360). Today nothing asks, and nothing sends.

If something you used on 0.2.x stops working after you move, that is a bug I want to know about, not a documented change.
