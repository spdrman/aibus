# aibus mcp

This file holds the parts of the MCP server that have their own page. The tool table and the capability flags are in epic #95 and in the other sections that land with the tools.

## Streamable HTTP (`--http`)

Stdio is the default and needs nothing. Some clients cannot spawn a process, and for those `aibus mcp --http 127.0.0.1:<port>` serves the same tools over Streamable HTTP, on one endpoint, `/mcp` (POST and GET). It is opt-in by flag and it is loopback only: any address that is not in `127.0.0.0/8` or `::1` is a usage error (exit 2) before a socket or the audit file is created, and there is no flag to override that. Port `0` asks the system for one, and the bound address is the first line on stdout:

```
aibus mcp listening on http://127.0.0.1:53124/mcp
```

Every request needs the bearer token:

* `AIBUS_MCP_TOKEN` from the environment (at least 16 visible ASCII characters), or
* with that unset, 32 random bytes as hex, made at start and printed once to stderr as the exact line to paste, `Authorization: Bearer <token>`.

The token is never written to disk, config, the audit log or any resource, it is registered with the redactor, and the comparison is constant time (both sides are hashed first, so even the length stays out of it). A request without the exact token is 401 with `WWW-Authenticate: Bearer`, before anything else about the request is looked at.

Past the token:

* `Host` must be the address aibus listens on (or `localhost`, `127.0.0.1`, `[::1]` with that port), or the answer is 403. That is the DNS rebinding check.
* `Origin`, when present, must be `http://127.0.0.1:<port>`, `http://localhost:<port>`, `http://[::1]:<port>` or a value given with `--origin <value>` (repeatable); anything else is 403. A request with no `Origin` is fine, since non-browser clients do not send one.
* `MCP-Protocol-Version` is checked and an unsupported value is 400. Both revisions work: `2026-07-28` is stateless with no `Mcp-Session-Id`, and `2025-11-25` opens with `initialize` and gets a session.

The rate limit and the audit log are the ones stdio has; an audit line gains `addr`, the client's socket address. SIGINT and SIGTERM stop the server and end every open stream.

The implementation sits in the `aibus` binary (`crates/aibus/src/mcp_http.rs`) on rmcp's `transport-streamable-http-server`. `aibus-mcp` has no socket and no HTTP stack, and a workspace test keeps it that way.

### A client on another machine

Do not bind a LAN address. Keep the server on loopback and forward a port over SSH from the machine that has the client:

```
ssh -N -L 7777:127.0.0.1:7777 me@the-aibus-machine
```

with `aibus mcp --http 127.0.0.1:7777` running on that machine. The client talks to `http://127.0.0.1:7777/mcp` on its own side with the same bearer token, and the request still arrives on the loopback interface, so every check above holds. Put the token in the client's configuration, not on the command line of a shared machine.

A full OAuth 2.1 resource server is a non-goal: aibus has one user, so one token.
