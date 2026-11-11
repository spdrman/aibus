# Providers

What the real providers (B1.5) read from the config and from disk. The account fields are in 04 section 4; this is the part that is specific to the code.

## Accounts

| Provider id | Crate | Default `base_url` | Notes |
|---|---|---|---|
| `anthropic` | `aibus-provider-anthropic` | `https://api.anthropic.com` | Messages API. `auth = { env = ... }`, `keyring` or `file` send `x-api-key`. Leave `/v1` off the `base_url`. |
| `openai` | `aibus-provider-openai` | `https://api.openai.com/v1` | Chat Completions by default; `api = "responses"` on the account switches that account to the Responses API. |
| `kimi` | `aibus-provider-openai` | `https://api.moonshot.ai/v1` | Chat Completions only (`api = "responses"` is a config error). Its own model and price table. |
| `openai-compatible` | `aibus-provider-openai` | none, `base_url` is required | Ollama, vLLM, LM Studio. `auth = { none = true }` sends no key at all. |

One provider id is one client for all its accounts, so two enabled accounts with the same `provider` must agree on `base_url` and `api`. If they do not, startup stops and names both accounts, because picking one would send the other account's key to the wrong server. Two local servers therefore need two ids, which `openai-compatible` does not offer yet (see the PR).

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

### The catalog

The built-in table is not inside a provider crate any more: it is `catalog/<provider>.json` at the repo root, one file per provider, and the crates `include_str!` it from there, so there is exactly one copy. A priced row always writes all four buckets (`cache_write_per_mtok: null` means the provider has no separate rate, which is different from a key that was forgotten). `crates/aibus-testkit/tests/catalog.rs` runs in the default test run and checks that every provider has a file, that every file parses, that no crate keeps a second copy, and that the files are in the layout the sync tool writes.

`catalog-sync` (`cargo run -p aibus-testkit --bin catalog-sync -- --snapshot api.json`) refreshes the files from a saved copy of the [models.dev](https://models.dev) data set. It never fetches anything, it is not part of the build, and the binary never runs it: I save the snapshot myself (the licence has to be confirmed before the first sync, see D-F8 in `docs/DECISIONS.md`), run the tool, read `git diff` and commit. It maps `limit.context` and `limit.output` to the sizes, `cost.input`, `output`, `cache_read` and `cache_write` to the four buckets, and `tool_call`, `reasoning` and `modalities.input` to a `capabilities` object. By default it only updates models that are already in the catalog (`--add-new` adds the rest), it leaves rows with a `price_from` alone, it prints a note per change and writes nothing without `--write`, and it refuses to write when a model would lose a price bucket it has today unless you pass `--allow-price-removal`. Prices change only through a reviewed PR.

A row can carry `price_from` (epoch seconds). Put the announced price in a second row with the same `id` and the date it starts, and `price()` picks the row with the latest `price_from` that has started, by the wall clock of the request. Usage that is already in the graph keeps the `cost_usd` it was given, it is never repriced. Your own `models.json` still wins over both rows.

For a gateway that reports what a request cost (OpenRouter, an `openai-compatible` account whose `base_url` is on `openrouter.ai`), the request asks for it with `usage: {"include": true}`, `usage.cost` in the response replaces the table's estimate, and `Usage.cost_source` is `provider` instead of `table`, so a dashboard can mark it. A response without `usage.cost` falls back to the table. Nothing changes at run time otherwise: the API's model list still refreshes into `~/.cache/aibus/models/`, and aibus still fetches no third-party catalog.

The sizes for a model that only the API list knows (no table entry) are a guess (128k context, 8k output; 32k and 4k for `openai-compatible`). Put the real numbers in your `models.json`.

## Tests

`cargo test` runs everything against a local mock server and the cassettes under each crate's `tests/cassettes/`. The nightly suite against a real account is `cargo test -p aibus-provider-anthropic --features live-tests` (and the same for `-openai`), with `AIBUS_LIVE_ACCOUNT=<name>` naming the account and its key in `AIBUS_LIVE_KEY_<NAME>` (name upper-cased, `-` turned into `_`). With `AIBUS_RECORD=1` the live tests also write what they saw to `tests/cassettes/live/<case>.http`, with the key and the account's ids stripped, ready to be reviewed and moved up a directory.

## OAuth

Off by default: v1 accounts use API keys. A provider takes part only when its factory lists `AuthMethodKind::OAuth` in `auth_methods`, and none of the built-in ones does yet, so today `aibus account add NAME --provider ID --oauth` stops with "has no OAuth login". The machinery is in place for the first one that does.

For an account that has it, the config line is `auth = { oauth = true }`. The access and refresh tokens are one JSON value in the keychain, in the entry a key would use (`aibus` / `account:NAME`), never in the config file, the graph, logs, transcripts or snapshots. Before each request the credential store checks the expiry and, when the access token has 60 seconds or less left, trades the refresh token for new ones and writes them back. If that fails (no refresh token, no flow, the provider says no) the request stops with an auth error (exit 4) telling you to run `aibus account add NAME --oauth` again. The token is sent as `Authorization: Bearer ...` like any bearer credential. The login (device code or loopback) sits behind the `OAuthFlow` trait in `aibus-core`; tests use `FakeOAuth`.
