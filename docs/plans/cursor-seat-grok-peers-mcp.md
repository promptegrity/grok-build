# Plan: make `grok-peers` MCP actually attach to `/model-cursor` seats

**Status:** implemented.
**Owner:** this workspace.
**Scope:** `xai-grok-peers`, `xai-grok-cursor-sdk`, `xai-grok-pager`, `xai-grok-tools`.

## Summary

`/model-cursor` already asks the Cursor SDK to attach a `grok-peers` stdio MCP server at
`CreateAgent` time, and the MCP server itself works. The attach fails because our stdio
server **writes LSP-style `Content-Length` framing on stdout**, which the MCP stdio
transport forbids — the spec (and every official SDK client, including Cursor's) reads
stdout as newline-delimited JSON. Cursor's client therefore fails to parse our very first
`initialize` response, drops the server, and the agent ends up with zero MCP servers, which
is exactly what the seat reports (`GetMcpTools` → "MCP server does not exist", only
`mcp_auth` visible). Two secondary defects in the same path would bite immediately after
the framing fix: we send `cwd: ""` in `StdioMcpServerConfig`, and we pass an `env` map with
only the two `GROK_PEER_*` variables, which on a TS-SDK-style stdio client **replaces** the
child environment instead of extending it (no `HOME`, no `PATH`, no `GROK_HOME` → the child
cannot find `~/.grok/peers`).

**Recommended approach: A — fix the stdio attach path** (framing, `cwd`, `env`, re-inject
`mcp_servers` on every `Send`, and fail the seat loudly when the MCP does not come up),
with **C (custom-tool fallback) deferred** to a follow-up only if A cannot be made reliable
against the pinned bridge. Rationale in [Recommended approach](#recommended-approach).

---

## Background / repro

### What the user sees

On a live `/model-cursor` seat (Cursor `composer-2.5` driven through the Grok TUI):

- `GetMcpTools` fails with "MCP server does not exist" / "failed during live tool discovery".
- The only MCP-related tool visible to the Cursor agent is `mcp_auth`.
- "MCP authentication is not supported in local SDK run."
- Cursor cannot call `list_peers` / `send_message` through `CallMcpTool`.
- Workaround that *does* work: drive `supergrok peers-mcp` by hand over stdio with
  `GROK_PEER_SESSION_ID` and `GROK_PEER_NAME=decisions` set.

The workaround is the important clue: the peer bus, the tool implementations, and the
process launch are all fine. Only the **client-visible wire behaviour** and the **attach
config** are broken.

### The path as it exists today

| Step | Location |
|---|---|
| `/model-cursor <model>` → `Action::ActivateCursorClient` | `crates/codegen/xai-grok-pager/src/slash/commands/model_cursor.rs:55` |
| dispatch → `Effect::CreateCursorAgent` (resolves `session_id`, `peer_name` via `peers::live_name`) | `crates/codegen/xai-grok-pager/src/app/dispatch/cursor_client.rs:42` |
| `create_cursor_agent` (resolves `grok_bin` from `current_exe()`, fallback `"grok"`) | `crates/codegen/xai-grok-pager/src/app/effects/mod.rs:4807` |
| same shape from the Grok tool `cursor_create_agent` | `crates/codegen/xai-grok-tools/src/implementations/cursor/mod.rs:256` |
| `create_local_agent_with` → `AgentOptions.mcp_servers` | `crates/codegen/xai-grok-cursor-sdk/src/client.rs:74` |
| `peers_mcp_servers` builds the `grok-peers` stdio entry | `crates/codegen/xai-grok-cursor-sdk/src/client.rs:344` |
| `send_with_progress` builds `SendOptions` **without** `mcp_servers` | `crates/codegen/xai-grok-cursor-sdk/src/client.rs:123` |
| hidden CLI `grok peers-mcp` | `crates/codegen/xai-grok-pager/src/app/cli.rs:34`, `.../peers_mcp_cmd.rs`, `xai-grok-pager-bin/src/main.rs:2153` |
| stdio MCP server (`initialize`, `tools/list`, `tools/call`) | `crates/codegen/xai-grok-peers/src/mcp.rs` |
| peer bus on disk (`$GROK_HOME/peers/`) | `crates/codegen/xai-grok-peers/src/{lib,inbox,registry}.rs` |
| proto pin (bridge v1.0.28) | `third_party/cursor-sdk-bridge/proto/sdk/v1/sdk_messages.proto` |

### Direct evidence gathered while writing this plan

Probing the installed binary the same way Cursor would:

```sh
printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n{"jsonrpc":"2.0","id":2,"method":"tools/list"}\n' \
  | GROK_PEER_SESSION_ID=probe GROK_PEER_NAME=probe supergrok peers-mcp
```

Result: exit 0, **0 bytes on stderr**, ~0.6 s wall clock, and stdout begins

```text
Content-Length: 147\r\n
\r\n
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05", ...}}Content-Length: 1613\r\n
...
```

So: the server starts fast, stdout is otherwise clean, `tools/list` returns both tools —
and the framing is wrong.

The MCP stdio transport spec is explicit ("Messages are delimited by newlines, and MUST NOT
contain embedded newlines"; "The server MUST NOT write anything to its `stdout` that is not
a valid MCP message"). `Content-Length` framing is an LSP idiom that MCP never adopted; the
official TypeScript, Go, Python and C# SDK clients are all NDJSON-only and treat each
stdout line as one JSON message. A client fed `Content-Length: 147` as a line raises a
parse error on the transport before `initialize` ever resolves.

---

## Root-cause hypotheses, ranked

### H1 — Non-compliant stdout framing (Content-Length instead of NDJSON). Confidence: high

`write_mcp_message` (`crates/codegen/xai-grok-peers/src/mcp.rs:233`) always emits
`Content-Length: N\r\n\r\n{json}`:

```rust
fn write_mcp_message(stdout: &mut impl Write, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write!(stdout, "Content-Length: {}\r\n\r\n", body.len())?;
    stdout.write_all(&body)?;
    stdout.flush()
}
```

The read side is already tolerant of both framings (`read_mcp_message`,
`crates/codegen/xai-grok-peers/src/mcp.rs:198`), which is why the manual workaround "works"
— a human or a lenient harness reads the output, not a strict MCP client. Cursor's client
is strict.

This single defect fully explains the observed symptom set: no `grok-peers` server, no
tools, and a generic "failed during live tool discovery"/"server does not exist" from
`GetMcpTools`.

**How to confirm:** the probe above. Also run the reference client against the binary
(`npx @modelcontextprotocol/inspector --cli supergrok peers-mcp --method tools/list`, with
`GROK_PEER_*` set); it should fail today and pass after the fix.

### H2 — `env` map replaces rather than extends the child environment. Confidence: high (post-H1 blocker)

In `peers_mcp_servers` (`crates/codegen/xai-grok-cursor-sdk/src/client.rs:344`):

```rust
    let mut env = std::collections::HashMap::new();
    env.insert("GROK_PEER_SESSION_ID".to_string(), id.session_id.clone());
    env.insert("GROK_PEER_NAME".to_string(), id.peer_name.clone());
```

MCP stdio clients built on the official SDK use `env: params.env ?? getDefaultEnvironment()`
— when the caller supplies an `env`, that map becomes the child's **entire** environment.
The spawned `grok peers-mcp` would then run without `HOME`, `PATH` or `GROK_HOME`.
`xai_grok_config::grok_home()` (`crates/codegen/xai-grok-config/src/paths.rs:34`) falls back
to `$HOME/.grok`, so the child would look at the wrong (or an unresolvable) peers directory
and `list_peers` would return an empty list even when the transport is healthy.

**How to confirm:** after H1 is fixed, add a temporary `stderr` line in `peers_mcp_cmd::run`
printing `GROK_HOME`, `HOME`, `PATH` and the resolved `peers_dir()`; read it from the bridge
child's stderr. Or reproduce locally with `env -i GROK_PEER_SESSION_ID=x GROK_PEER_NAME=y supergrok peers-mcp`.

### H3 — `cwd: ""` makes the spawn fail. Confidence: medium-high

`peers_mcp_servers` sets `cwd: String::new()`. Proto3 cannot distinguish "empty" from
"unset" for a `string` field, so the bridge sees `""`. Depending on how it forwards that to
`child_process.spawn`, an empty-string `cwd` is either ignored (harmless) or passed through
to `uv_spawn`, which fails with `ENOENT` before the process starts — a silent MCP startup
failure with no user-visible error anywhere in Grok.

**How to confirm:** set `cwd` to the seat workspace and see whether behaviour changes; or
inspect the bridge's own log/stderr while creating an agent.

### H4 — MCP config never re-injected on `Send`. Confidence: medium (latent, will bite later)

In `send_with_progress` (`crates/codegen/xai-grok-cursor-sdk/src/client.rs:123`):

```rust
                options: Some(SendOptions {
                    mode: agent_mode_i32(plan_mode),
                    ..Default::default()
                }),
```

Cursor documents that inline `mcpServers` are **not persisted across `Agent.resume()`** and
that per-send `mcpServers` fully replaces creation-time servers for that run. Our
`CursorSdkClient` never calls `ResumeAgent`, so today this is not the primary cause — but
the bridge is a shared, lazily-spawned sidecar (`crates/codegen/xai-grok-cursor-sdk/src/bridge.rs:98`)
and any bridge restart, agent reload, or future resume path would silently lose the server.
Re-passing the same map on every `Send` is cheap insurance and makes the attach idempotent.

### H5 — `grok_bin` from `current_exe()` is not spawnable. Confidence: low-medium

`create_cursor_agent` uses `std::env::current_exe()` with a `"grok"` fallback
(`crates/codegen/xai-grok-pager/src/app/effects/mod.rs:4816`). Failure modes: running from
`target/debug|release/xai-grok-pager` on a machine where the dev binary is later rebuilt
mid-session; a path containing spaces if the bridge ever shell-splits the command; the
`"grok"` fallback when `grok` is not on the child's `PATH` (guaranteed once H2 applies); a
`~/.local/bin/supergrok` copy that is replaced by `install -m 755` while the seat is live.
On the reporter's machine `supergrok peers-mcp` runs fine, so this is not the current
blocker — but the resolution deserves hardening (verify the resolved path exists, is a
regular file, and is executable; otherwise fall back to a `PATH` lookup of `grok`).

### H6 — Server startup cost / stray stdout from the pager `main()`. Confidence: low

`grok peers-mcp` goes through the full `xai-grok-pager-bin` `main()` before reaching the
MCP loop: crash-handler install, Sentry init, user-guide doc extraction, `validate_requirements`
(which can `exit(2)` with a message), `apply_sandbox`, telemetry. Measured at ~0.6 s with
clean stdout today, so it is within any reasonable client timeout — but it is fragile:
anything that ever prints to **stdout** on that path (a future notice, an update banner)
would corrupt the stream, and `validate_requirements` failing would look like a silent MCP
crash. Worth an explicit guard.

### H7 — Approval / auth classification, `settingSources`, sandbox. Confidence: low

The seat reports `mcp_auth` and "MCP authentication is not supported in local SDK run".
That is consistent with *no* servers being attached (only the generic auth meta-tool
remains) rather than with our server being classified as needing OAuth — a stdio server has
no auth flow. `LocalAgentOptions.setting_sources` and `sandbox_options` are never set
anywhere in the repo (`grep` for `setting_sources` / `sandbox_options` over `*.rs` is
empty), so we load inline servers only and pull in no user-level HTTP MCPs that could
require auth. **Keep it that way**: leave `setting_sources` empty. Cursor's known limitation
(sandbox approval provider blocking MCP calls that need approval; "local SDK runs cannot
request interactive approval") only becomes relevant if we later enable sandboxing or
setting sources.

### H8 — `protocolVersion` echo. Confidence: low, but fix while we are here

`initialize` always answers `"2024-11-05"` (`crates/codegen/xai-grok-peers/src/mcp.rs:14`)
regardless of what the client requested. `2024-11-05` is still accepted by current clients,
but the correct behaviour is to echo the client's requested version when we support it and
otherwise return our latest. Cheap to make correct and removes a future breakage.

---

## Recommended approach

**Primary: A — repair the stdio MCP attach path.**

Concretely: NDJSON framing on stdout, a real `cwd`, an `env` that carries `GROK_HOME` (and
that assumes replacement semantics), a spawnability check on `grok_bin`, `mcp_servers`
re-passed on every `Send`, `setting_sources` left empty, and a visible failure in the TUI
when the seat comes up without `grok-peers`.

**Why A:**

1. The evidence points at a one-line protocol bug plus two config bugs, not at a design
   problem. The MCP server, the tool schemas, the peer bus, and the process launch are all
   verified working. Replacing the transport because of a framing bug would be a large
   rewrite motivated by a small defect.
2. A keeps the product surface Cursor users expect: a named `grok-peers` MCP server that
   shows up in `GetMcpTools`, is inspectable with the MCP Inspector, and behaves like every
   other MCP server. Custom tools appear under the synthetic `custom-user-tools` server,
   which is a different (and less discoverable) surface.
3. `grok peers-mcp` is already a shipped, hidden CLI with its own tests. Fixing it also
   fixes any other MCP client someone points at it.
4. Cost: roughly one small file change plus config, versus implementing a whole gRPC
   callback service for B.

**Fallback: C — hybrid, custom tools as a second channel.** If, after A, the pinned bridge
still refuses to surface `grok-peers` (e.g. it turns out the bridge does not forward
`AgentOptions.mcp_servers` for local agents at v1.0.28), fall back to registering
`list_peers` / `send_message` as `LocalAgentOptions.custom_tools` and implementing
`SdkCustomToolCallbackService` in `xai-grok-cursor-sdk`. Custom tools are attractive
precisely where MCP is weak — they skip interactive approval and work in sandboxed /
auto-review runs — and execution stays in the Grok process, so there is no child process,
no environment inheritance question, and no framing at all.

**Why not B as primary:** it is the larger change (compile
`sdk_custom_tool_callback_service.proto`, stand up a loopback Connect *server* inside
`xai-grok-cursor-sdk` — the crate is client-only today — wire `SetToolCallback` or pass the
callback address at bridge spawn, then re-implement the two tool bodies against
`xai_grok_peers`), it introduces a new inbound-server attack surface on loopback, and it
would leave the real bug in `grok peers-mcp` shipped and broken for every other MCP client.
Do it as a deliberate follow-up if A proves insufficient or if we later need sandboxed
seats, not as the first attempt.

---

## Implementation steps

Five PR-sized steps. PR 1 alone is expected to fix the reported symptom; PRs 2–3 remove the
next two blockers; 4–5 are hardening and docs.

### PR 1 — Make the stdio MCP server spec-compliant

Files:

- `crates/codegen/xai-grok-peers/src/mcp.rs`

Changes:

1. `write_mcp_message`: emit `serde_json::to_string(msg)` + `\n`, no headers. Guarantee no
   embedded newlines (compact serialization already does; assert it in a test).
2. Keep `read_mcp_message` tolerant of both framings (harmless, and lets the old manual
   workaround keep working), but add a test that a bare NDJSON line round-trips.
3. `initialize`: echo `params.protocolVersion` when it is a version we support, else return
   our latest; keep `capabilities: { tools: {} }` and `serverInfo`.
4. Reject `tools/call` for unknown tools with an `isError` content block (already correct);
   leave notifications unanswered (already correct).

Tests (unit, in-module):

- `write_mcp_message` produces exactly one line ending in `\n`, no `Content-Length`, and the
  line parses as JSON.
- Round-trip: feed two NDJSON lines through `run_peers_mcp_stdio_with` over an in-memory
  duplex and assert two NDJSON responses with matching ids.
- `initialize` echoes a client-requested `2025-06-18`; falls back for an unknown version.

### PR 2 — Fix the MCP server config we hand to Cursor

Files:

- `crates/codegen/xai-grok-cursor-sdk/src/client.rs` (`PeersMcpIdentity`, `peers_mcp_servers`)
- `crates/codegen/xai-grok-pager/src/app/effects/mod.rs` (`create_cursor_agent`)
- `crates/codegen/xai-grok-tools/src/implementations/cursor/mod.rs` (`cursor_create_agent`)

Changes:

1. Add `cwd: String` and `grok_home: String` to `PeersMcpIdentity`; both callers populate
   them (`cwd` = the seat workspace already available at the call site, `grok_home` =
   `xai_grok_config::grok_home()`).
2. `peers_mcp_servers` sets `cwd` to that workspace instead of `String::new()`.
3. Build the `env` map assuming **replacement** semantics: `GROK_PEER_SESSION_ID`,
   `GROK_PEER_NAME`, `GROK_HOME`, plus forwarded `HOME`, `PATH`, and (on Windows)
   `USERPROFILE`/`SYSTEMROOT`/`APPDATA`, `TMPDIR`/`TEMP`, `LANG`. Forward only when present
   in the parent environment. This is a superset that is also correct under merge semantics.
4. Extract `resolve_grok_bin()` into `xai-grok-cursor-sdk` (or a shared helper) so both
   call sites share it: `current_exe()` → verify the path exists and is a regular file →
   otherwise `which("grok")` → otherwise `"grok"`. Return the verified absolute path.

Tests:

- Extend `peers_mcp_stdio_config` (`client.rs:560`-ish test module): asserts `command` is
  non-empty and absolute, `args == ["peers-mcp"]`, `cwd` non-empty, and that the `env` map
  contains `GROK_PEER_SESSION_ID`, `GROK_PEER_NAME`, `GROK_HOME`, `PATH`, `HOME`.
- `resolve_grok_bin` unit tests mirroring the existing `discover.rs` style: env/current-exe
  present, current-exe missing → `PATH` fallback, nothing → `"grok"`.

### PR 3 — Re-inject `mcp_servers` on every `Send`

Files:

- `crates/codegen/xai-grok-cursor-sdk/src/client.rs`

Changes:

1. Store the `Option<PeersMcpIdentity>` per agent on the client (a small
   `Mutex<HashMap<agent_id, PeersMcpIdentity>>`), populated by `create_local_agent_with` and
   cleared by `delete_agent`.
2. `send_with_progress` sets `SendOptions.mcp_servers = peers_mcp_servers(&identity)` when
   an identity is known for that `agent_id`. Because per-send servers *replace* the
   creation-time set, always send the full map, never a partial one.
3. No behaviour change when no identity is registered (empty map == unset on the wire).

Tests:

- Unit: after `create_local_agent_with` registers an identity, the map returned for that
  agent id is non-empty and equals the creation-time map; after `delete_agent`, empty.
- A request-building unit test asserting `SendOptions.mcp_servers` contains `grok-peers`
  (factor the `SendRequest` construction into a pure helper so it is testable without a
  live bridge).

### PR 4 — Fail loudly instead of silently seating without peers

Files:

- `crates/codegen/xai-grok-pager/src/app/effects/mod.rs` (`create_cursor_agent`)
- `crates/codegen/xai-grok-pager/src/cursor_client.rs` (activation / system messages)
- `crates/codegen/xai-grok-pager-bin/src/main.rs` (`Command::PeersMcp` arm)

Changes:

1. Preflight in `create_cursor_agent`: before `CreateAgent`, spawn the resolved
   `grok_bin peers-mcp` with the exact env/cwd we are about to send, write an `initialize`
   + `tools/list` over NDJSON, and require both tools back within a short timeout (2 s).
   On failure, surface a system scrollback block naming the resolved binary and the reason,
   and refuse to enter client mode (or enter it with an explicit "peer messaging is
   unavailable" banner — pick one and state it in the docs; refusing is the honest default
   given the whole point of the seat is peer messaging).
2. Harden the `peers-mcp` arm: run it as early as possible in `main()` (before doc
   extraction, Sentry and crash-handler install) so startup cost and any future stdout
   writes cannot corrupt the stream, and make sure `init_tracing_simple` only ever targets
   stderr on this path.
3. Add a `--selftest` (hidden) flag or an env-gated mode to `peers-mcp` that performs the
   handshake against itself and exits non-zero on failure, so the preflight and CI can share
   one code path.

Tests:

- Integration test in `xai-grok-peers` (or `xai-grok-pager`) that spawns the built binary
  with `peers-mcp`, speaks NDJSON, and asserts `initialize` + `tools/list` return both
  tools. Gate on `PAGER_BINARY` like the scripted-scenario tests so it does not force a
  rebuild.
- Test that a bogus `grok_bin` produces the actionable error string rather than a seat.

### PR 5 — Docs, only after the behaviour is real

Files:

- `crates/codegen/xai-grok-pager/docs/user-guide/26-cursor-sdk-bridge.md`
- `crates/codegen/xai-grok-pager/docs/user-guide/25-cross-session-messaging.md`

Changes: describe what actually happens (server name `grok-peers`, the two tools, the env
the child receives, what the seat does when the MCP fails to start, and the fact that
`~/.cursor/mcp.json` is never involved). Do not touch these until PRs 1–4 are verified on a
live two-terminal run.

---

## Test / verification plan

### Automated

| Level | What | Where |
|---|---|---|
| Unit | NDJSON framing, `initialize` version echo, notification handling | `xai-grok-peers/src/mcp.rs` tests |
| Unit | `peers_mcp_servers` shape: absolute `command`, non-empty `cwd`, env includes `GROK_HOME`/`PATH`/`HOME` | `xai-grok-cursor-sdk/src/client.rs` tests |
| Unit | `resolve_grok_bin` fallbacks | `xai-grok-cursor-sdk` |
| Unit | `SendOptions.mcp_servers` populated for a known agent | `xai-grok-cursor-sdk` |
| Integration | spawn the real binary, NDJSON handshake, `tools/list` contains both tools | new test, `PAGER_BINARY`-gated |
| Integration (best effort) | live bridge: `CreateAgent` with the peers MCP, then a `Send` whose prompt asks the agent to call `list_peers`; assert the run reports a tool call | `#[ignore]`d, requires a Cursor key |

Commands (per `AGENTS.md`, always crate-scoped):

```sh
cargo test -p xai-grok-peers
cargo test -p xai-grok-cursor-sdk
cargo fmt --all --check && cargo clippy -p xai-grok-peers -p xai-grok-cursor-sdk
```

### Manual: reference MCP client

```sh
GROK_PEER_SESSION_ID=test GROK_PEER_NAME=probe \
  npx @modelcontextprotocol/inspector --cli ./target/release/xai-grok-pager peers-mcp \
  --method tools/list
```

Must list `list_peers` and `send_message`. This is the single check that would have caught
H1 before shipping. Also verify a minimal environment still works:

```sh
env -i GROK_PEER_SESSION_ID=test GROK_PEER_NAME=probe HOME="$HOME" PATH="$PATH" \
  ./target/release/xai-grok-pager peers-mcp < /dev/null
```

### Manual: two-terminal seat test

1. Terminal 1: `grok --name coder` (normal Grok coding agent).
2. Terminal 2: `grok --name cursor`, then `/model-cursor composer-2` (or whatever
   `cursor_list_models` offers). The status bar should show `Cursor · {model}`.
3. In terminal 2, prompt the Cursor seat: *"list your MCP servers and tools"*. Expected:
   `grok-peers` present with `list_peers` and `send_message`; `GetMcpTools` no longer errors.
4. In terminal 2: *"call list_peers"*. Expected: a peer named `coder` with its cwd — this
   also proves `GROK_HOME` reached the child (H2).
5. In terminal 1, ask Grok to send a message to the peer named `cursor`. The seat should
   receive it and reply via `send_message`, and terminal 1 should show the reply **without**
   the pager's one-shot fallback relay kicking in.
6. Leave with `/model <grok-model>`; confirm the Cursor agent is deleted and the peer note
   (`cursor:<model>`) is cleared.
7. Negative test: point the seat at a bogus binary (temporarily, via the resolver's env
   override) and confirm the seat refuses to start with an actionable message instead of
   seating silently without peers.

---

## Risks

- **Cursor SDK / bridge semantics change.** The pin is `third_party/cursor-sdk-bridge/VERSION`
  = v1.0.28. Inline MCP attach, per-send replacement semantics, and env handling are all
  bridge behaviour we do not control. Mitigation: the PR 4 preflight turns any future
  regression into a clear startup error instead of a mystery, and the integration test
  pins the observable contract.
- **`env` semantics assumption.** PR 2 assumes replacement and sends a superset. If the
  bridge merges instead, we have merely re-exported variables the child already had —
  harmless. If it replaces *and* we missed a variable some platform needs, the child may
  still misbehave; the preflight catches it before the seat opens.
- **`current_exe()` under cargo vs an installed `supergrok`.** A seat started from
  `target/debug/xai-grok-pager` pins that path for the session; rebuilding mid-session can
  invalidate it on some platforms. The resolver verifies the path at attach time, but a
  long-lived seat can still go stale. Acceptable; document it.
- **Sandbox.** We leave `sandbox_options` unset and `setting_sources` empty. If either is
  enabled later, Cursor's known limitation (local SDK runs cannot request interactive
  approval, and the sandbox approval provider blocks MCP calls needing approval) may block
  `grok-peers` calls. That is the scenario where fallback C becomes the right answer.
- **Choosing A means not implementing custom-tool callbacks.** If a future requirement
  (sandboxed seats, auto-review runs) needs approval-free tools, B still has to be built.
  PR 3's per-agent identity map is deliberately shaped so a custom-tool path can reuse it.
- **Preflight cost.** Spawning a probe process adds ~0.6 s to `/model-cursor`. Acceptable
  for a mode switch; keep the timeout tight and the failure message specific.

## Non-goals

- Redesigning the peer bus (`$GROK_HOME/peers/`, registry, unix sockets, inbox modes).
- Requiring users to edit `~/.cursor/mcp.json` or any file-based MCP configuration.
- Depending on MCP OAuth or any interactive MCP approval flow.
- Cloud Cursor agents, `ResumeAgent`, or broadening the SDK client's RPC surface beyond
  what the fix needs.
- Changing the `list_peers` / `send_message` tool schemas or their prompt-shaping
  descriptions.
