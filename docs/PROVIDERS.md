# Providers

This is the page about how aibus reaches a model: what the pieces are, what each wire does differently, how a credential gets there, and where the model lists come from. The account fields are in 04 section 4 of the spec; this is the part that is specific to the code. `docs/CAPABILITIES.md` is the generated table of what every preset can do, and `docs/MIGRATION.md` is the page for a 0.1.0 config.

## The connection model

Four things are involved in one request, and they are separate on purpose:

| Concept | What it is | Where it lives |
|---|---|---|
| Account | A name, a provider, an auth, optional aliases, tags, rate limits and a fallback list. What a run is bound to. | `[[account]]` in the user config |
| Provider definition | An id, an api, a base URL, the env vars that usually hold the key, the default capabilities, the compat flags, a model table. | A preset (`crates/aibus-provider/src/presets.json`), or a `[[provider]]` in the user config, or both |
| Api | The wire protocol, and the only part that is code: `anthropic-messages`, `anthropic-bedrock`, `anthropic-vertex`, `openai-chat`, `openai-responses`, `gemini-generate-content`. | One adapter crate each (see below) |
| Auth | Where the credential comes from and how it reaches the request: `keyring`, `env`, `file`, `none`, `oauth`, `sigv4`, `gcloud`, `aws_env`, `aws_profile`. | `auth = { ... }` on the account |

An account says `provider = "<id>"`, the id names a definition, the definition names an api, and the api picks the adapter. `aibus-core` never names a provider. A vendor that speaks one of the six wires is a few lines of data and no new crate. The registry builds one instance per provider id, so an id carries its URL: two Ollama servers are two ids, and nothing conflicts.

| Adapter crate | Serves |
|---|---|
| `aibus-provider-anthropic` | `anthropic-messages`, `anthropic-bedrock`, `anthropic-vertex` |
| `aibus-provider-openai` | `openai-chat`, `openai-responses` |
| `aibus-provider-gemini` | `gemini-generate-content` |

There are three ways to connect: pick a preset (nothing to write beyond the account), write a `[[provider]]` for a server the presets do not know, or log in with OAuth where the vendor publishes a flow for third-party applications (see "Auth kinds").

## Providers

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
| `bedrock` | `anthropic-bedrock` | by region | `AWS_BEARER_TOKEN_BEDROCK` | Claude on Amazon Bedrock. See "Claude on Bedrock and Vertex". |
| `vertex` | `anthropic-vertex` | by region | none (`gcloud`) | Claude on Google Vertex AI. |
| `gemini` | `gemini-generate-content` | `https://generativelanguage.googleapis.com` | `GEMINI_API_KEY` | Google's native API (D-14). Key in the `x-goog-api-key` header. Keeps thought signatures, shows thought summaries, counts tokens, has a priced model table. Leave `/v1beta` off the `base_url`. Vertex for Gemini is not served. |
| `gemini-openai` | `openai-chat` | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` | Google's `/v1beta/openai` shim, a subset of Chat Completions: no thought signatures, no context cache, and it answers `store` with a 400. Use `gemini` when you need more than plain chat. |

A model table is the built-in list for that vendor (a start, sizes only, no prices unless I know one), refreshed from the API and overridable as described under "Model tables". A model that is not listed still resolves when its id starts with one of the preset's `prefixes`.

### `[[provider]]`

For a server the presets do not know, or to change a preset (a gateway in front of Groq, an extra header):

```toml
[[provider]]
id = "ollama-home"
api = "openai-chat"                 # anthropic-messages | openai-chat | openai-responses | gemini-generate-content
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
* `base_url` must start with `http://` or `https://`, and `api` must be one of the four.

`aibus doctor` lists every configured provider with its wire, its URL and where the definition came from (a preset, a `[[provider]]`, or both). `aibus account add NAME --provider groq` asks for the key and names `GROQ_API_KEY` as the environment alternative (`--env GROQ_API_KEY`).

### Old configs

A 0.1.0 config loads unchanged. `docs/MIGRATION.md` has the three cases that change shape.

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

## The apis

### anthropic-messages

`aibus-provider-anthropic`, the direct Messages API. `POST {base_url}/v1/messages` with the key in `x-api-key` and an `anthropic-version` header; leave `/v1` off the `base_url`. The system prompt is the `system` field, `cache_hint` becomes `cache_control: ephemeral` breakpoints, thinking blocks keep their signature and go back unchanged (a thinking block without one is dropped, since the API would refuse it), and `effort` travels in the beta header. Token counting uses `/v1/messages/count_tokens`, and the model list refreshes from `/v1/models`. Used by `anthropic`.

### openai-chat

`aibus-provider-openai`, Chat Completions. `POST {base_url}/chat/completions` with `Authorization: Bearer <key>` (or the definition's own header, `api-key` on Azure), `stream_options.include_usage` on every streaming call, and the output limit under the key `compat` names (`max_tokens` or `max_completion_tokens`). Every preset except `anthropic`, `gemini`, `bedrock` and `vertex` uses it, and most of what differs between them is a compat flag. There is no token counting endpoint, so the estimate is used. Used by `openai`, `kimi` and the OpenAI-shaped vendors.

### openai-responses

The Responses API on the same adapter: `api = "openai-responses"` on a `[[provider]]`, or `api = "responses"` on an `openai` account (the old form). The conversation is sent whole every time with `store: false`, so nothing depends on the server keeping state; `no_store` and `no_strict_tools` cover servers that refuse those keys. A provider whose server has no Responses API says `responses = false` and the loader refuses the api for it.

### gemini-generate-content (native Gemini)

`aibus-provider-gemini` serves `gemini-generate-content` (D-14). The request goes to `POST {base_url}/v1beta/models/<model>:streamGenerateContent?alt=sse`, the key travels only in the `x-goog-api-key` header (never in the URL), and the model list and `countTokens` use the same host.

* **Request.** The system prompt is `systemInstruction`. Tools are `functionDeclarations`, with `$schema`, `additionalProperties` and `default` stripped from the schema at every depth (a parameter that is itself called `default` stays) and `parameters` left out for a tool that takes none, since Gemini refuses an object with no properties. `ToolChoice` becomes `functionCallingConfig` (`AUTO`, `NONE`, `ANY`, and `ANY` with `allowedFunctionNames` for a named tool). A thinking budget becomes `thinkingConfig { thinkingBudget, includeThoughts: true }` on every model except the 2.0 and 1.x families, which refuse it.
* **Stream.** Text parts are text, `thought: true` parts are thinking, and a `functionCall` part is one whole call (start, one input fragment, end). Gemini says `STOP` for a reply that called tools; aibus reports `ToolUse` then. A `thoughtSignature` on a call is kept on the `ToolUse` block (`signature`) and sent back unchanged with the call in the next request, which Gemini 3 insists on.
* **Usage.** Input is `promptTokenCount` minus `cachedContentTokenCount`, output is `candidatesTokenCount` plus `thoughtsTokenCount`, cache read is the cached count, `reasoning_tokens` is the thoughts count. Caching is implicit on Google's side, so `cache_hint` is dropped.
* **Stops.** `MAX_TOKENS` is max tokens; `SAFETY`, `RECITATION`, `PROHIBITED_CONTENT`, `BLOCKLIST`, `SPII` and a blocked prompt are `ContentFilter`; `MALFORMED_FUNCTION_CALL` is an `InvalidRequest` that carries Google's raw text.
* **Errors.** Google's `{ error: { code, status, message } }` maps by status: 429 `RESOURCE_EXHAUSTED` is `RateLimit` (the wait comes from `error.details[].retryDelay`, since there is no header), 401 and 403 are `Auth`, a 400 "API key not valid" is `Auth` too (Google does not send a 401 for it), 404 is `ModelNotFound`, and a 400 about the input token count is `ContextTooLong`.
* **Models.** `catalog/gemini.json` has gemini-3-pro-preview, 2.5-pro, 2.5-flash, 2.5-flash-lite, 2.0-flash and 2.0-flash-lite with context window, max output and `reasoning`. Prices are Google's list prices for prompts up to 200k tokens, the tier aibus prices (long-context tiers are out of scope); the F8 sync is where they get re-checked. A dated snapshot id (`gemini-2.5-flash-20250617`) has the undated price, and any `gemini-` id resolves with no price until the table knows it. `refresh_models` reads `/v1beta/models`, keeps what can `generateContent`, and takes the sizes Google reports for models the table has not heard of.

### anthropic-bedrock and anthropic-vertex (Claude on Bedrock and Vertex)

Both are off by default: the plain `anthropic` provider never talks to either. They are two more `api` values of `aibus-provider-anthropic`, `anthropic-bedrock` and `anthropic-vertex`, behind the `bedrock` and `vertex` presets, so a config picks them like any other provider:

```toml
[[account]]
name = "bedrock"
provider = "bedrock"
auth = { sigv4 = true }        # or { aws_env = true } or { aws_profile = "work" }
region = "us-east-1"

[[account]]
name = "vertex"
provider = "vertex"
auth = { gcloud = true }
project = "my-project"
region = "us-east5"            # "global" for the global endpoint
```

`region` and `project` can also sit on a `[[provider]]` with the same id; the account's own wins. The loader refuses `sigv4`, `aws_env` and `aws_profile` on a provider whose api is not `anthropic-bedrock`, and `gcloud` on one that is not `anthropic-vertex`, with the line. `aibus account add NAME --provider bedrock --region us-east-1 --key-stdin` writes the table for a keychain key (`project` has a flag too); the `aws_*` and `gcloud` forms go in the config by hand.

| | Bedrock | Vertex |
|---|---|---|
| Default host | `https://bedrock-runtime.<region>.amazonaws.com` | `https://<region>-aiplatform.googleapis.com` (`global` has no region prefix) |
| Endpoint | `POST /model/<id>/invoke-with-response-stream` | `POST /v1/projects/<project>/locations/<region>/publishers/anthropic/models/<id>:streamRawPredict` |
| Auth | SigV4 over the request, or `Authorization: Bearer` for a Bedrock API key | `Authorization: Bearer <token>`, asked for on every request |
| Stream | AWS event stream, each chunk a base64 Messages API event | the usual SSE |
| Model ids | `anthropic.claude-...-v1:0` or an inference profile such as `us.anthropic.claude-...` | `claude-sonnet-4-5@20250929` |

Credentials for Bedrock, one source per account:

* `sigv4`: the keychain entry holds `ACCESS_KEY_ID:SECRET_ACCESS_KEY[:SESSION_TOKEN]` (`account add --key-stdin` stores it; the `keyring` it writes means the same thing).
* `aws_env`: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN` are read when a request is made, and never stored.
* `aws_profile = "name"`: static keys from `~/.aws/credentials` (or `AWS_SHARED_CREDENTIALS_FILE`). A file readable by anyone but you is refused with the `chmod 600` hint. There is no SSO, no `credential_process` and no instance metadata.
* A Bedrock API key, `AWS_BEARER_TOKEN_BEDROCK`, goes through `auth = { env = "AWS_BEARER_TOKEN_BEDROCK" }`. A secret with no colon in it is sent as a bearer token and nothing is signed.

For Vertex, the token is `GOOGLE_OAUTH_ACCESS_TOKEN` when that is set, else the output of `gcloud auth print-access-token`. `gcloud` is found on `PATH` and run with no shell, killed after 10 seconds, with its stderr dropped; its output is treated as a secret, kept 50 minutes in memory under the account's name, and never written to disk or to a log.

The signer is about sixty lines on `hmac` and `sha2` (already in the lock file) instead of the AWS SDK, which would add dozens of crates and a second HTTP stack to `cargo deny`. Neither transport has a token counting endpoint (the estimate is used) or a model list, so `aibus account test` makes one real request instead: one output token on the smallest model, and it shows the cost. `aibus doctor` shows each provider's region and project.

Prices come from the plain Claude rows through `price_of`: `catalog/bedrock.json` lists the Bedrock ids and inference profiles (`anthropic.`, `us.`, `global.`), and `catalog/vertex.json` the `name@date` ids, each pointing at the plain `claude-` row it costs the same as. A model you add to your own `models.json` can say `"price_of": "claude-sonnet-4-5"` too, or carry a `price` of its own, which wins.

## Auth kinds

One source per account. The credential never goes in the config file: `auth` names where it lives.

| Kind | Where the credential is | Notes |
|---|---|---|
| `keyring` | The OS keychain entry `aibus` / `account:NAME`, written by `aibus account add` or `/login`. | The default for an interactive account. |
| `env` | The environment variable the account names, read when a request is made and never stored. | `AIBUS_ACCOUNT_<NAME>_KEY` when the account names none. |
| `file` | A `0600` file that holds the key. | A file readable by anyone else is refused. |
| `none` | No credential. | Local servers. |
| `oauth` | Access and refresh tokens, one JSON value in the keychain entry a key would use. | See "OAuth". |
| `sigv4` | `ACCESS_KEY_ID:SECRET_ACCESS_KEY[:SESSION_TOKEN]` in the keychain entry. | Bedrock only. |
| `aws_env` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, read at request time. | Bedrock only. |
| `aws_profile` | Static keys of a profile in a `0600` `~/.aws/credentials`. | Bedrock only. No SSO, no `credential_process`. |
| `gcloud` | An access token from `GOOGLE_OAUTH_ACCESS_TOKEN` or `gcloud auth print-access-token`, cached 50 minutes in memory. | Vertex only. |

The loader refuses a cloud form on an api it does not belong to, with the line, and refuses two sources on one account.

### OAuth

Off by default: accounts use API keys unless the provider publishes a login for third-party applications (the rule is in `docs/SECURITY.md`, "Identity and logins"). Two generic flows exist in `crates/aibus/src/oauth/`, driven by a definition (kind, authorize and token URLs, optional client id, scopes, callback port, whether the server echoes `state`, and whether the result is tokens or a key): `pkce_loopback` (RFC 7636 with S256 and a one-request listener on 127.0.0.1, five minute timeout, the address is printed and, with a terminal, opened with the platform opener; pasting the redirect address also works) and `device_code` (RFC 8628, 5 s interval, `slow_down` adds 5 s).

For an account that has it, the config line is `auth = { oauth = true }`. The access and refresh tokens are one JSON value in the keychain, in the entry a key would use (`aibus` / `account:NAME`), never in the config file, the graph, logs, transcripts or snapshots. Before each request the credential store checks the expiry and, when the access token has 60 seconds or less left, trades the refresh token for new ones and writes them back. If that fails (no refresh token, no flow, the provider says no) the request stops with an auth error (exit 4) telling you to run `aibus account add NAME --oauth` again. The token is sent as `Authorization: Bearer ...` like any bearer credential. The login (device code or loopback) sits behind the `OAuthFlow` trait in `aibus-core`; tests use `FakeOAuth`.

### OpenRouter

`aibus account add NAME --provider openrouter --oauth` (the `openrouter` preset). It needs no client registration. The login returns a durable API key, which is stored like any key, so the account ends up as `auth = { keyring = true }` and there is nothing to refresh. The callback tries port 3000 first and falls back to a free one.

### Token logins

For a provider whose login returns tokens, the config line is `auth = { oauth = true }`. The access and refresh tokens are one JSON value in the keychain, in the entry a key would use (`aibus` / `account:NAME`), never in the config file, the graph, logs, transcripts or snapshots. Before each request the credential store checks the expiry and, when the access token has 60 seconds or less left, trades the refresh token for new ones and writes them back. If that fails (no refresh token, no flow, the provider says no) the request stops with an auth error (exit 4) telling you to run `aibus account add NAME --oauth` again. The token is sent as `Authorization: Bearer ...` like any bearer credential. The login sits behind the async `OAuthFlow` trait in `aibus-core` (every call takes a cancel token); tests use `FakeOAuth` and `aibus_testkit::oauth::FakeAuthServer`. Two requests that find an expired token share one refresh: a lock per account, and the second reads what the first stored. No provider uses token logins yet, so only `account add` has a flow wired in.

## Model tables and the catalog


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

### The catalog

The built-in table is not inside a provider crate any more: it is `catalog/<name>.json` at the repo root, one file per table a preset names (`model_table` in `presets.json` is now always a name), and `aibus_provider::catalog::table` is the one place that `include_str!`s them, so there is exactly one copy. A preset that ships no models (a local server, Azure) names `empty` and has no file. A priced row always writes all four buckets (`cache_write_per_mtok: null` means the provider has no separate rate, which is different from a key that was forgotten). `crates/aibus-testkit/tests/catalog.rs` runs in the default test run and checks that every provider has a file, that every file parses, that no crate keeps a second copy, and that the files are in the layout the sync tool writes.

`catalog-sync` (`cargo run -p aibus-testkit --bin catalog-sync -- --snapshot api.json`) refreshes the files from a saved copy of the [models.dev](https://models.dev) data set. It never fetches anything, it is not part of the build, and the binary never runs it: I save the snapshot myself (the licence has to be confirmed before the first sync, see D-F8 in `docs/DECISIONS.md`), run the tool, read `git diff` and commit. It maps `limit.context` and `limit.output` to the sizes, `cost.input`, `output`, `cache_read` and `cache_write` to the four buckets, and onto aibus's own names: `tool_call` is `capabilities.tools`, an `image` among `modalities.input` is `capabilities.images`, and `reasoning` is the row's `reasoning`. A capability I set by hand (`max_tools`, `logprobs`) stays, and the keys an earlier version wrote under models.dev's names are dropped. By default it only updates models that are already in the catalog (`--add-new` adds the rest), it leaves rows with a `price_from` alone, it prints a note per change and writes nothing without `--write`, and it refuses to write when a model would lose a price bucket it has today unless you pass `--allow-price-removal`. Prices change only through a reviewed PR.

A row can carry `price_from` (epoch seconds). Put the announced price in a second row with the same `id` and the date it starts, and `price()` picks the row with the latest `price_from` that has started, by the wall clock of the request. Usage that is already in the graph keeps the `cost_usd` it was given, it is never repriced. Your own `models.json` still wins over both rows.

For a gateway that reports what a request cost (the `openrouter` preset, or any definition whose `base_url` is on `openrouter.ai`), the request asks for it with `usage: {"include": true}`, `usage.cost` in the response replaces the table's estimate, and `Usage.cost_source` is `provider` instead of `table`, so a dashboard can mark it. A response without `usage.cost` falls back to the table. Nothing changes at run time otherwise: the API's model list still refreshes into `~/.cache/aibus/models/`, and aibus still fetches no third-party catalog.

The sizes for a model that only the API list knows (no table entry) are a guess (128k context, 8k output; 32k and 4k for `openai-compatible`). Put the real numbers in your `models.json`.

## Capability matrix

`docs/CAPABILITIES.md` has one row per preset: its api, the auth forms it takes, whether its adapter reports tools, parallel tool calls, thinking, effort, images, prompt cache and logprobs by default, whether it can refresh its model list and count tokens, how many rows its catalog file has, and its compat flags. It is generated by `crates/aibus-testkit/tests/capability_matrix.rs` from the presets, the real adapters and the catalog, and the same test fails when the committed page is out of date, so it cannot drift. A catalog row can still override any capability for one model.

## Account fallback

An account can name others to try when it fails before the first token of a turn:

```toml
[defaults]
fallback_on = ["quota", "auth", "overloaded"]   # the default

[[account]]
name = "anthropic-work"
provider = "anthropic"
fallback = ["anthropic-personal", "bedrock"]
```

The run asks the next account the same thing, with the model resolved again there (an account that cannot serve it, or has no key, is skipped with a log line). It never switches after the first `TextDelta`, `ThinkingDelta` or `ToolUseStart`, because the user has already seen output from the first account. The five retries of a retryable error are per account. The account that gave way sits out of selection for a while (a quota until its window resets or 30 minutes, an overload 60 seconds, a failed login until `aibus account test` passes), and that is kept in memory only. The run's header shows `↷ anthropic-personal`. An agent definition can set `fallback: []` to stay put. The advisor and the router fall back the same way. The full rules are in 04 section 4.3.

## Tests

`cargo test` runs everything against a local mock server and the cassettes under each crate's `tests/cassettes/`. The nightly suite against a real account is `cargo test -p aibus-provider-anthropic --features live-tests` (and the same for `-openai`), with `AIBUS_LIVE_ACCOUNT=<name>` naming the account and its key in `AIBUS_LIVE_KEY_<NAME>` (name upper-cased, `-` turned into `_`). With `AIBUS_RECORD=1` the live tests also write what they saw to `tests/cassettes/live/<case>.http`, with the key and the account's ids stripped, ready to be reviewed and moved up a directory.
