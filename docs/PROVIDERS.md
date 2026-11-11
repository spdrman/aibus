# Providers

What the real providers (B1.5) read from the config and from disk. The account fields are in 04 section 4; this is the part that is specific to the code.

## Providers are data

A provider is a definition, `{ id, api, base_url, env_vars, auth_methods, model_table, prefixes, default_capabilities, compat, headers, query }`. The `api` is the wire: `anthropic-messages`, `openai-chat` or `openai-responses`, and that is the only part that is code (`aibus-provider-anthropic` serves the first, `aibus-provider-openai` the other two). Everything else is one reviewed file, `crates/aibus-provider/src/presets.json`, so a vendor that speaks one of those wires is a few lines of data and no new crate. `aibus-core` never names a provider.

An account says `provider = "<id>"`. The registry builds one instance per id, so an id carries its URL: two Ollama servers are two ids, and nothing conflicts.

### Presets

Nothing to write for these:

| Provider id | Wire | Default `base_url` | Key variable | Notes |
|---|---|---|---|---|
| `anthropic` | `anthropic-messages` | `https://api.anthropic.com` | `ANTHROPIC_API_KEY` | Messages API. Leave `/v1` off the `base_url`. |
| `openai` | `openai-chat` | `https://api.openai.com/v1` | `OPENAI_API_KEY` | `api = "openai-responses"` switches it to the Responses API. |
| `kimi` | `openai-chat` | `https://api.moonshot.ai/v1` | `MOONSHOT_API_KEY` | Chat Completions only. Its own model and price table. |
| `openai-compatible` | `openai-chat` | yours | none | Kept for migration. `base_url` is required; `auth = { none = true }` sends no key. |
| `groq` | `openai-chat` | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` |  |
| `mistral` | `openai-chat` | `https://api.mistral.ai/v1` | `MISTRAL_API_KEY` |  |
| `deepseek` | `openai-chat` | `https://api.deepseek.com/v1` | `DEEPSEEK_API_KEY` |  |
| `xai` | `openai-chat` | `https://api.x.ai/v1` | `XAI_API_KEY` |  |
| `together` | `openai-chat` | `https://api.together.xyz/v1` | `TOGETHER_API_KEY` |  |
| `fireworks` | `openai-chat` | `https://api.fireworks.ai/inference/v1` | `FIREWORKS_API_KEY` |  |
| `cerebras` | `openai-chat` | `https://api.cerebras.ai/v1` | `CEREBRAS_API_KEY` |  |
| `deepinfra` | `openai-chat` | `https://api.deepinfra.com/v1/openai` | `DEEPINFRA_API_KEY` |  |
| `perplexity` | `openai-chat` | `https://api.perplexity.ai` | `PERPLEXITY_API_KEY` | Search models; no `/v1` in the path. |
| `openrouter` | `openai-chat` | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |  |
| `azure-openai` | `openai-chat` | yours | `AZURE_OPENAI_API_KEY` | Key in the `api-key` header, `?api-version=` on every call. `base_url` is your deployment: `https://<resource>.openai.azure.com/openai/deployments/<deployment>`. |
| `ollama` | `openai-chat` | `http://127.0.0.1:11434/v1` | none | Local, no key needed. |
| `lm-studio` | `openai-chat` | `http://127.0.0.1:1234/v1` | none | Local, no key needed. |
| `vllm` | `openai-chat` | `http://127.0.0.1:8000/v1` | `VLLM_API_KEY` | Local, no key needed. |
| `llama-cpp` | `openai-chat` | `http://127.0.0.1:8080/v1` | none | Local, no key needed. |
| `litellm` | `openai-chat` | `http://127.0.0.1:4000/v1` | `LITELLM_API_KEY` | Local, no key needed. |
| `gemini-openai` | `openai-chat` | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` | Google's `/v1beta/openai` shim, a subset of Chat Completions. |

A model table is the built-in list for that vendor (a start, sizes only, no prices unless I know one), refreshed from the API and overridable as described under "Model tables". A model that is not listed still resolves when its id starts with one of the preset's `prefixes`.

### `[[provider]]`

For a server the presets do not know, or to change a preset (a gateway in front of Groq, an extra header):

```toml
[[provider]]
id = "ollama-home"
api = "openai-chat"                 # anthropic-messages | openai-chat | openai-responses
base_url = "http://10.0.0.5:11434/v1"
headers = { "x-team" = "home" }     # never a secret

[[account]]
name = "home"
provider = "ollama-home"
auth = { none = true }
```

Rules:

* A `[[provider]]` with the id of a preset overrides that preset field by field: what you leave out stays the preset's. `headers`, `query` and `default_capabilities` merge (a field you name replaces the preset's, the rest stays); `compat`, if you give it, replaces the preset's list whole.
* A new id with an OpenAI `api` starts from `openai-compatible` (open aliases, logprobs on, no cache or effort claims), so `id`, `api` and `base_url` are enough.
* `headers` and `query` are checked like `auth.key` is: a name that looks like a credential (`authorization`, `api-key`, anything with `auth`, `key`, `token`, `secret`, `cookie`) is rejected, because credentials do not go in config. Azure's `api-key` header is the preset's `auth_header`, not a header you write.
* `[[provider]]` is read from the user file only, like `[[account]]`: a project file cannot point a key at another server. It is ignored there with a warning.
* `base_url` must start with `http://` or `https://`, and `api` must be one of the three.

`aibus doctor` lists every configured provider with its wire, its URL and where the definition came from (a preset, a `[[provider]]`, or both). `aibus account add NAME --provider groq` asks for the key and names `GROQ_API_KEY` as the environment alternative (`--env GROQ_API_KEY`).

### Old configs

A 0.1.0 config loads unchanged (`fixtures/config/` has one per id). `base_url` and `api` on an account still work and are laid over the provider's definition. `provider = "openai-compatible"` with a `base_url` keeps working, and `aibus doctor` suggests the `[[provider]]` form. `api = "responses"` on an `openai` account is the same as `provider = "openai"` with `api = "openai-responses"`. Two accounts of one id that set different `base_url`s or `api`s in the old form are still an error, since one instance cannot serve both; the fix is two `[[provider]]` ids.

### What a model can do, and the wire quirks

Capabilities live on the model, with the provider definition supplying the default. A `[[provider]]` (or a preset) says what its models do unless told otherwise, and a model entry overrides only the fields it names:

```toml
[[provider]]
id = "home"
api = "openai-chat"
base_url = "http://10.0.0.5:8000/v1"
default_capabilities = { tools = true, logprobs = true }
compat = ["no_store", { max_tokens_field = "max_completion_tokens" }]
```

```json
{ "models": [
  { "id": "gpt-5", "context_window": 400000, "max_output": 128000, "family": "gpt-5", "deprecated": false,
    "reasoning": true, "capabilities": { "effort": true, "logprobs": false } }
] }
```

`capabilities` has `tools`, `parallel_tool_calls`, `thinking`, `effort`, `images`, `prompt_cache`, `logprobs`, `max_tools` and `system_role` (`separate` or `first_message`), each optional. `reasoning` says the model emits thinking or reasoning tokens at all; `capabilities.thinking` says a request may ask for a thinking budget, which is a different thing. The older `effort: true` on an entry still works and means `capabilities.effort`. A model's capabilities resolve in the same order as its table: the provider's `default_capabilities`, then the built-in table, then the cache, then your `models.json`, field by field, so an override file that only fixes a context window keeps what the built-in table said about tools. `degrade` and the router's "prefers logprobs" rule both read the resolved per-model capabilities (`Provider::capabilities_for`).

`compat` is the closed list of wire quirks that are not capabilities. A flag is a bare name, or a one-key table where it takes a value. Anything not in this list is not supported yet; it goes on the epic's "Later" list, never into the enum.

| Flag | What it changes in the request |
|---|---|
| `no_strict_tools` | Responses tools go out without the `strict` key. |
| `no_developer_role` | The `developer` role is sent as `system`. |
| `max_tokens_field` | `{ max_tokens_field = "max_tokens" }` or `"max_completion_tokens"`: the key the output limit goes in. |
| `reasoning_field` | `{ reasoning_field = "reasoning_effort" }` or `"reasoning"` (sent as `reasoning: { effort }`): where the effort goes. |
| `reasoning_content_replay` | Earlier thinking goes back to the server as `reasoning_content` (Kimi). |
| `no_usage_in_stream` | No `stream_options.include_usage`. |
| `no_tool_choice` | `tool_choice` is never sent. |
| `no_named_tool_choice` | A named tool choice goes out as "any tool" (`required`, or `any` on the Messages wire). |
| `no_store` | `store` is left out of a Responses body. |
| `no_sampling_params` | `temperature` is left out. |
| `no_system_role` | No system role: the prompt is folded into the first user message. |
| `strip_images` | Pictures are replaced by a note. |

Every flag has a fixture under `tests/fixtures/compat/` in the crate that honours it (`aibus-provider-openai` and `aibus-provider-anthropic`) recording the request with the flag and without. A preset that claims a flag is run against that fixture.

## Claude on Bedrock and Vertex

Both are off by default: the plain `anthropic` provider never talks to either. They live in `aibus-provider-anthropic` and are built with `Anthropic::bedrock(cfg, region, clock)` or `Anthropic::vertex(cfg, project, region, token_source)`, with the account's `base_url` overriding the default host.

| | Bedrock (`auth = sigv4`) | Vertex (`auth = gcloud`) |
|---|---|---|
| Default host | `https://bedrock-runtime.<region>.amazonaws.com` | `https://<region>-aiplatform.googleapis.com` (`global` has no region prefix) |
| Endpoint | `POST /model/<id>/invoke-with-response-stream` | `POST /v1/projects/<project>/locations/<region>/publishers/anthropic/models/<id>:streamRawPredict` |
| Auth | SigV4 over the request, from the account secret `ACCESS_KEY_ID:SECRET_ACCESS_KEY[:SESSION_TOKEN]` | `Authorization: Bearer <token>` from a `TokenSource` (for example `gcloud auth print-access-token`), asked for on every request |
| Stream | AWS event stream, each chunk a base64 Messages API event | the usual SSE |
| Model ids | `anthropic.claude-...-v1:0` or an inference profile such as `us.anthropic.claude-...` | `claude-sonnet-4-5@20250929` |

The signer is about sixty lines on `hmac` and `sha2` (already in the lock file) instead of the AWS SDK, which would add dozens of crates and a second HTTP stack to `cargo deny`. Neither transport has a token counting endpoint (the estimate is used) or a model list, and `aibus account test` only checks that the secret parses or that a token can be had, with no network call. Prices are only known for the plain `claude-` ids, so a Bedrock id shows cost `n/a` until you add it to your `models.json`.

## Model tables

Each provider starts from a built-in table (`context_window`, `max_output`, `family`, `deprecated`, and a `price` where I know one). Three layers, later ones winning:

1. the built-in table;
2. `~/.cache/aibus/models/<provider>.json`, the list the API last returned, refreshed by `aibus account test` and, in the background, when it is older than 7 days (the first frame never waits for it);
3. `~/.config/aibus/providers/<provider>/models.json`, yours.

The files share one shape:

```json
{
  "models": [
    { "id": "my-model", "context_window": 32768, "max_output": 4096, "family": "mine",
      "deprecated": false,
      "price": { "input_per_mtok": 1.0, "output_per_mtok": 2.0, "cache_read_per_mtok": 0.1 } }
  ],
  "families": { "mine": 3.5 }
}
```

`families` is the characters per token for a family (default 4). The counter used when a provider has no counting endpoint is that ratio plus a 10 % margin, so `auto_compact_at` fires a little early rather than late. A model with no `price` costs `n/a`: tokens are counted and the cost stays 0, it is never guessed. A dated snapshot (`claude-sonnet-4-5-20250929`, `gpt-4.1-2025-04-14`) uses the price of the model it is a snapshot of. A model that is not in a table still resolves when its id starts with the provider's own prefix (`claude-`, `gpt-`, `kimi-`, ...); `openai-compatible` has no prefix, so its models come from the server's list or from your `models.json`, and an account alias can name anything.

The sizes for a model that only the API list knows (no table entry) are a guess (128k context, 8k output; 32k and 4k for `openai-compatible`). Put the real numbers in your `models.json`.

## Tests

`cargo test` runs everything against a local mock server and the cassettes under each crate's `tests/cassettes/`. The nightly suite against a real account is `cargo test -p aibus-provider-anthropic --features live-tests` (and the same for `-openai`), with `AIBUS_LIVE_ACCOUNT=<name>` naming the account and its key in `AIBUS_LIVE_KEY_<NAME>` (name upper-cased, `-` turned into `_`). With `AIBUS_RECORD=1` the live tests also write what they saw to `tests/cassettes/live/<case>.http`, with the key and the account's ids stripped, ready to be reviewed and moved up a directory.

## OAuth

Off by default: v1 accounts use API keys. A provider takes part only when its factory lists `AuthMethodKind::OAuth` in `auth_methods`, and none of the built-in ones does yet, so today `aibus account add NAME --provider ID --oauth` stops with "has no OAuth login". The machinery is in place for the first one that does.

For an account that has it, the config line is `auth = { oauth = true }`. The access and refresh tokens are one JSON value in the keychain, in the entry a key would use (`aibus` / `account:NAME`), never in the config file, the graph, logs, transcripts or snapshots. Before each request the credential store checks the expiry and, when the access token has 60 seconds or less left, trades the refresh token for new ones and writes them back. If that fails (no refresh token, no flow, the provider says no) the request stops with an auth error (exit 4) telling you to run `aibus account add NAME --oauth` again. The token is sent as `Authorization: Bearer ...` like any bearer credential. The login (device code or loopback) sits behind the `OAuthFlow` trait in `aibus-core`; tests use `FakeOAuth`.
