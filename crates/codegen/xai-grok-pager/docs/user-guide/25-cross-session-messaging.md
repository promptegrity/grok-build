# Cross-Session Messaging

Grok sessions on the same machine can discover each other and exchange plain-text messages. Use this when one session learns something another session needs mid-task — a schema change, a finished migration, or a decision — without copy-pasting between terminals.

This is **not** the same as [subagents](16-subagents.md): subagents are child workers inside one session. Cross-session messaging connects **independent** `grok` processes (or multiple sessions hosted by the same leader) that you start yourself.

---

## Requirements

- macOS or Linux (inbox sockets use Unix domain sockets; Windows named-pipe support is limited)
- Sessions must share the same Grok home (`~/.grok` or `$GROK_HOME`) so they can see the peer registry

---

## Name your sessions

Address messages by a human-readable name:

```bash
# terminal one
grok --name api

# terminal two
grok --name frontend
```

Short form: `grok -n api`.

If you omit `--name`, Grok derives a name from the working-directory folder plus a short session-id suffix (for example `my-app-3f`).

You can also rename a live session with [`/rename`](04-slash-commands.md):

```
/rename payments-api
```

When two live sessions would share a name, Grok allocates a variant such as `api-2`.

---

## See who is reachable

In either session:

```
/peers
```

Aliases: `/list-agents`, `/list_agents`.

The listing shows each live peer's name, working directory, and a short session id. Your current session is marked `(this session)`.

The model can also call the `list_peers` tool on its own when it needs to find a target.

---

## Send a message

Tell Grok what the other session should know. You do not call the tools yourself:

```text
Explain what we just changed to the session named frontend
```

```text
Ask api whether the migration finished
```

Grok uses `list_peers` to find the target and `send_message` to deliver plain text. The receiving session sees the message as an injected turn, labeled with the sender's name and a reply address.

### Keep messages useful

Each inbound peer message starts a full model turn. Agents should send only when the other session needs a concrete work question, a request, a decision, or a fact to continue — not greetings, thanks, status recaps, availability offers, or "what are you working on?" check-ins. After sending, they stop; they do not follow up to confirm receipt. Without that, two sessions can spend tokens on a polite ping-pong.

### What a message can and cannot do

- **Can**: pass a finding, status, or decision as plain text
- **Cannot**: transfer files or conversation history; approve permission prompts; change configuration; run slash commands embedded in the text (they arrive as literal text)

Permission boundaries stay per-session. The receiving Grok still prompts you for any action that needs approval there.

---

## How it works

1. Each session registers a row under `~/.grok/peers/` and binds a local inbox socket.
2. `list_peers` / `/peers` reads that registry (stale PIDs are pruned).
3. `send_message` connects to the target inbox and writes one JSON line.
4. The receiver injects the text into the next agent turn (or mid-turn via interjection).

If the receiving session is in [Cursor client mode](26-cursor-sdk-bridge.md) (`/model-cursor`), it does **not** start a Grok turn. The pager is a TUI for the bound Cursor agent: it forwards the peer message (with a `send_message` reply address) and Cursor replies on the peer bus itself via the attached `grok-peers` MCP (`list_peers` / `send_message`). `list_peers` / `/peers` show a `note` such as `cursor:composer-2` on that session. If Cursor does not call `send_message`, the seat falls back to one automatic relay so the sender is not left waiting.

Same-machine delivery never leaves your machine.

Containers that do not share `~/.grok` cannot see each other's peers.

---

## Related

- [Session Management](17-sessions.md) — resume, rename, storage layout
- [Subagents](16-subagents.md) — in-session parallel workers
- [Agent Dashboard](23-dashboard.md) — switch between sessions in one pager
