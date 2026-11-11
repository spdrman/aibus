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
