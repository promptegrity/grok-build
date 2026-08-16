# Cursor SDK Bridge

Grok can drive [Cursor agents](https://cursor.com/docs/sdk/bridge) through the official `cursor-sdk-bridge` sidecar. This is **not** messaging an open Cursor IDE window: Grok becomes a client of Cursor's local agent runtime.

Cursor usage is billed on your **Cursor** account (SDK tag on the [usage dashboard](https://cursor.com/dashboard/usage)), not xAI.

`/model` in Grok still selects Grok/xAI models. The usual way to talk to Cursor is **`/model-cursor`**: that session becomes a thin client and forwards prompts (and peer messages) to a local Cursor agent. The `cursor_*` tools remain available on a normal Grok coder as an advanced path.

---

## Prerequisites

1. A Cursor user or service [API key](https://cursor.com/dashboard/api).
2. The `cursor-sdk-bridge` sidecar next to `grok` (shipped with current installs) **or** `CURSOR_SDK_BRIDGE_BIN` pointing at a downloaded binary.

```bash
grok login-cursor --api-key "key_..."
# or: export CURSOR_API_KEY="key_..."
```

`grok login-cursor` stores the key in `~/.grok/auth.json` under `cursor::api_key` (owner-only). It never writes `xai::api_key` or `XAI_API_KEY`.

```bash
grok logout-cursor   # clears only the Cursor key
grok logout          # xAI session; does not remove the Cursor key
```

---

## Sidecar layout

After `install.sh` / `grok update` / npm postinstall:

```text
~/.grok/bin/grok
~/.grok/bin/cursor-sdk-bridge
```

Discovery order:

1. `CURSOR_SDK_BRIDGE_BIN`
2. Sibling of the running `grok` binary
3. `~/.grok/bin/cursor-sdk-bridge`
4. `PATH`

If the sidecar is missing (older install, or Windows arm64 — no upstream build), tools fail with an actionable error. Developers can fetch the pinned release:

```bash
crates/codegen/xai-grok-pager/scripts/fetch-cursor-sdk-bridge.sh
```

The pin lives in `third_party/cursor-sdk-bridge/VERSION`.

---

## Client mode (`/model-cursor`)

In a second terminal (after `grok login-cursor`):

```
/model-cursor
```

Pick a Cursor model from the dropdown (or type `/model-cursor composer-2`). Grok creates a local Cursor agent in the current workspace and attaches a `grok-peers` MCP (`list_peers` / `send_message`) under this session's peer name. The MCP is a stdio child of `grok peers-mcp` with `GROK_PEER_SESSION_ID`, `GROK_PEER_NAME`, and `GROK_HOME` set; it does **not** read `~/.cursor/mcp.json`. If that child fails its initialize/`tools/list` handshake, `/model-cursor` refuses to enter client mode and prints the resolved binary plus the error. After that:

- Each prompt you type is sent to Cursor and the session waits for the reply. The Grok model is not in the loop. While Cursor works, this TUI shows a spinner with the current step and streams the reply as it arrives.
- `/plan` (or Shift+Tab) on this session puts Cursor in its native plan mode for the next send. This is Cursor's plan mode, not Grok's `plan.md` approval overlay.
- Peer messages from another Grok session are forwarded the same way. Cursor should call `send_message` only when it has a question, a request, or a fact the named peer needs — not greetings or acknowledgements. This session does not send a second reply of its own (one fallback relay only if Cursor never called `send_message`).
- `/model <grok-name>`, `/new` (`/clear`), `/home`, or `/quit` (`/exit`) leaves client mode and **deletes** the Cursor agent created for this session.

The status bar shows `Cursor · {model}` while the mode is active.

Typical two-terminal setup:

1. Terminal 1: `grok --name coder` — normal Grok coding agent.
2. Terminal 2: `grok --name cursor` then `/model-cursor` — Cursor seat (this TUI).
3. In terminal 1, ask Grok to message the peer named `cursor`. Cursor sees the message and replies with `send_message`.

Cancel (Esc) does not stop an in-flight Cursor run in this version.

---

## Tools

These tools are on the grok-build presets (including concise, hashline, and plan):

| Tool | Purpose |
|------|---------|
| `cursor_list_models` | Models available to your Cursor key |
| `cursor_create_agent` | Create a **local** agent in the current workspace |
| `cursor_send` | Send a prompt and **block** until Cursor finishes (do not poll) |
| `cursor_list_agents` | Resume an existing agent (`agent_id`) |

Typical flow: list models → create agent → send (repeat `cursor_send` with the same `agent_id` for multi-turn).

Cloud Cursor agents and custom tool/store callbacks are not in this integration.
