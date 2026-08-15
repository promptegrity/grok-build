# Cursor SDK Bridge

Grok can drive [Cursor agents](https://cursor.com/docs/sdk/bridge) through the official `cursor-sdk-bridge` sidecar. This is **not** messaging an open Cursor IDE window: Grok becomes a client of Cursor's local agent runtime.

Cursor usage is billed on your **Cursor** account (SDK tag on the [usage dashboard](https://cursor.com/dashboard/usage)), not xAI.

`/model` in Grok still selects Grok/xAI models. Cursor model ids come from `cursor_list_models`.

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

## Tools

These tools are on the grok-build presets (including concise, hashline, and plan):

| Tool | Purpose |
|------|---------|
| `cursor_list_models` | Models available to your Cursor key |
| `cursor_create_agent` | Create a **local** agent in the current workspace |
| `cursor_send` | Send a prompt and wait for the run text |
| `cursor_list_agents` | Resume an existing agent (`agent_id`) |

Typical flow: list models → create agent → send (repeat `cursor_send` with the same `agent_id` for multi-turn).

Cloud Cursor agents and custom tool/store callbacks are not in this integration.
