![aibus — AI agent orchestration](assets/aibus-logo.svg)

A terminal app for running AI agents with sub-agents, an advisor and continuity across sessions. Rust, ratatui, rataflow, crossterm.

![The aibus dashboard in a 10 second session: a lead plans, the advisor rail speaks, the router answers, three workers fill their cards one after the other, then a card is selected and its menu opens](docs/media/aibus-ui.gif)

That is the dashboard on the built-in fake provider, so no account or network is involved: the lead makes a plan, the advisor rail on the left comments on it, the router answers a small decision, three workers take a card each, and the last two seconds are the mouse (hover, click, right click). The recording is made by a script, see [docs/media/README.md](docs/media/README.md).

What it does, in one paragraph: a lead agent talks to you and delegates to sub-agents defined as Markdown files with YAML frontmatter (project or user scope, tool allow-lists, model per agent, background or foreground, resumable). An advisor reads the whole session and speaks only at checkpoints. A router answers small decisions with a cheap model and escalates the uncertain ones. Ledgers, handoffs and memory carry state from one session to the next... compound, don't compact. Twenty-four provider presets (Anthropic, OpenAI, Gemini, Claude on Bedrock and Vertex, Groq, Ollama and the rest, see [docs/CAPABILITIES.md](docs/CAPABILITIES.md)) and three ways to connect: pick a preset, write a `[[provider]]` for a server the presets do not know, or log in with OAuth where a vendor publishes a login for third-party applications. Several accounts per provider, an optional fallback to the next account when one runs out of quota before the first token, and credentials in the OS keychain. Coming from 0.1.0? Your config loads as it is, and [docs/MIGRATION.md](docs/MIGRATION.md) has the three cases that change shape. The whole application state is one Causl graph, so every event is one atomic commit, the commit log is the audit trail, and a snapshot is the session save.

The spec is at [docs/SPEC.md](docs/SPEC.md) and the contested decisions are in [docs/DECISIONS.md](docs/DECISIONS.md).

## Install

Releases are built for macOS (arm64, x86_64), Linux (x86_64 musl and glibc, aarch64) and Windows (x86_64). Every installer checks the SHA-256 of the archive against the release's `SHA256SUMS` before it unpacks anything.

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/spdrman/aibus/main/install.sh | sh
```

Windows (PowerShell 5.1 or 7):

```powershell
irm https://raw.githubusercontent.com/spdrman/aibus/main/install.ps1 | iex
```

Homebrew (macOS and Linux), this repository is the tap:

```sh
brew tap spdrman/aibus https://github.com/spdrman/aibus && brew install spdrman/aibus/aibus
```

What the installers do:

* `install.sh` puts the binary in `$HOME/.local/bin` (no `sudo`). On Linux x86_64 it installs the musl build, which runs on any distro; `--gnu` asks for the glibc build, and aarch64 only has the glibc one. It uses `curl` or `wget`, and if `cosign` is on your `PATH` it also verifies the signature bundle (otherwise it says it skipped that). It never edits a shell startup file unless you pass `--modify-path`, and prints the `export PATH=...` line when the directory is not on your `PATH`.
* `install.ps1` puts `aibus.exe` in `%LOCALAPPDATA%\Programs\aibus\bin` and adds that directory to your user `PATH`. It checks the SHA-256 only (no cosign step). There is no arm64 build, so on Windows on Arm it installs the x86_64 one.
* Both read the same knobs: `AIBUS_VERSION` (for example `0.1.0`, default the latest release), `AIBUS_INSTALL_DIR`, and `AIBUS_NO_MODIFY_PATH=1`.

```sh
# a specific version, somewhere else
curl -fsSL https://raw.githubusercontent.com/spdrman/aibus/main/install.sh | AIBUS_VERSION=0.1.0 AIBUS_INSTALL_DIR=/opt/aibus/bin sh
```

### First checks

```sh
aibus version    # the release tag and the commit it was built from
aibus doctor     # the install and the config: keychain backend, config files, anything off
```

With no account configured, `aibus` starts on the built-in fake provider. Add a real one with `aibus account add`, see [docs/PROVIDERS.md](docs/PROVIDERS.md).

### Author workflows from an MCP client

`aibus mcp` is an MCP server, so an editor or chat app can read your workflows and check a draft. Three lines of config in the client:

```json
{"command": "aibus", "args": ["mcp"]}
```

By default it is read-only: it lists and explains workflows, validates a draft against your real agents, shows what a workflow would do for an input without calling a model, and serves the workflow schema and an authoring prompt. `--allow write` adds creating and updating workflows (a gate needs `--allow gates` as well), and `--allow run` adds starting, watching and cancelling runs under the tightest settings aibus has. Setup for a few clients, the tool list and what the server logs are in [docs/MCP.md](docs/MCP.md).

### Upgrade

Run the installer again (it replaces the binary in place), or `brew upgrade aibus`.

### Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/spdrman/aibus/main/install.sh | sh -s -- --uninstall
brew uninstall aibus && brew untap spdrman/aibus
```

On Windows, run the script as a script block with `-Uninstall`:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/spdrman/aibus/main/install.ps1))) -Uninstall
```

That removes the binary (and the `PATH` entry on Windows). Your config, session data and keychain entries stay: remove the accounts with `aibus account remove <name>` first if you want them gone.

### Manual download and verification

Each release has `aibus-<target>.tar.gz` (`.zip` on Windows), `SHA256SUMS`, `aibus.cdx.json` (the SBOM), and a `.sigstore.json` bundle next to every file. Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`.

```sh
sha256sum -c --ignore-missing SHA256SUMS      # shasum -a 256 -c on macOS
cosign verify-blob --bundle aibus-<target>.tar.gz.sigstore.json \
  --certificate-identity https://github.com/spdrman/aibus/.github/workflows/release.yml@refs/heads/release \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  aibus-<target>.tar.gz
tar -xzf aibus-<target>.tar.gz && ./aibus-<target>/aibus version
```

### Build from source

You need the Rust version in [rust-toolchain.toml](rust-toolchain.toml) (1.89, the MSRV), which `rustup` picks up by itself.

```sh
git clone https://github.com/spdrman/aibus && cd aibus
cargo build --release --locked
target/release/aibus version
```

### What is not proven

* No real provider has been exercised end to end: the tests and the demo run on the fake provider, and the live provider run is a separate nightly job.
* No live run against Gemini, Bedrock or Vertex has happened in CI. Those adapters are proven against recorded cassettes and a scripted server, the OAuth flows against a fake authorization server, and the Vertex token path against a stand-in for `gcloud`. The OpenRouter login in particular has not been tried against openrouter.ai from a release build.
* The capability columns for a preset are its adapter's default for a model its table says nothing about. A row of the catalog can differ, and a provider can change a model under me, so `docs/CAPABILITIES.md` is what aibus assumes, not what the vendor guarantees.
* Windows is unverified. The binary is built, but nobody has run it on a Windows machine yet (see [docs/RELEASE.md](docs/RELEASE.md)), and the `PATH` change in `install.ps1` is the part the container tests could not exercise.
* The release pipeline and the installers have only run against fake releases in containers until the first real release is cut.
* Only the Linux and macOS terminals I own have been tried by eye; the rest of the checklist in [docs/RELEASE.md](docs/RELEASE.md) is open.

## Layout

```
crates/aibus                      the binary
crates/aibus-core                 engine: definitions, runs, tools, hooks, continuity
crates/aibus-graph                Causl contract, mini engine, causl-engine-core host
crates/aibus-provider             Provider trait, accounts, fake provider
crates/aibus-provider-anthropic
crates/aibus-provider-openai      OpenAI, Kimi/Moonshot and OpenAI-compatible dialects
crates/aibus-provider-gemini      Google's native Gemini API
crates/aibus-tui                  ratatui widgets and view model
crates/aibus-testkit
```

## Licence

MIT, see [LICENSE](LICENSE).
