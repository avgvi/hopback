# JSON output

`hopback --json` (or `hb --json`) prints the matching sessions as **exactly one
JSON object on stdout**, followed by a newline, and exits. It never opens the
TUI and never starts an agent. Notices and errors go to stderr; when the
command fails, the exit status is non-zero and stdout is empty.

```bash
hb --json                           # the 50 newest sessions
hb --json "auth bug"                # search, same syntax as the TUI
hb --json --agent codex --limit 10  # filter and page
hb --json --all --no-refresh        # everything in the index, without scanning
```

The query takes the same free text and keywords as the TUI search line
(`agent:claude`, `dir:backend`, date keywords), and `--agent` / `--directory`
work as they do everywhere else.

## Schema, version 1

```json
{
  "schema_version": 1,
  "sessions": [
    {
      "id": "01a1cde9-b5f4-7141-b2c9-8a1edb368004",
      "agent": "codex",
      "title": "Uploader retries",
      "original_title": "add retry logic to the uploader",
      "directory": "/work/app",
      "timestamp": "2026-11-11T20:00:08Z",
      "message_count": 4,
      "pinned": true,
      "hidden": false,
      "forked_from": { "agent": "claude", "id": "5f0c2a9e-…" },
      "resume_command": ["codex", "resume", "01a1cde9-b5f4-7141-b2c9-8a1edb368004"]
    }
  ],
  "meta": {
    "state": "more",
    "total": 24,
    "offset": 0,
    "limit": 10,
    "returned": 10,
    "next_offset": 10
  }
}
```

### Session fields

| Field | Type | Meaning |
|---|---|---|
| `id` | string | The agent's own session id. Unique per agent, not across agents. |
| `agent` | string | Agent id: `claude`, `codex`, `opencode`, … Use `agent:id` as a selector in other hopback commands. |
| `title` | string | The title hopback shows: your local rename if you set one, otherwise the same as `original_title`. |
| `original_title` | string | The title derived from the agent's files. |
| `directory` | string | The working directory the session ran in. Resume from here. |
| `timestamp` | string | When the conversation started, RFC 3339 in UTC. |
| `message_count` | number | Messages hopback indexed for the session. |
| `pinned` | boolean | Pinned in hopback. |
| `hidden` | boolean | Hidden in hopback. Always `false` unless `--include-hidden` is given. |
| `forked_from` | object or null | `{ "agent", "id" }` of the session this one was forked from with hopback, else `null`. |
| `resume_command` | array of strings | The argv hopback would run to resume the session, to be run in `directory`. |

With `--include-messages`, each session also has `messages`: the prompts and
replies hopback indexed, oldest first, as `{ "role": "user" | "assistant",
"text": "…" }`. Tool calls and thinking are not part of it. Without the flag
the field is absent.

### `resume_command` and skip-permission flags

`resume_command` is the command exactly as hopback would run it. A session is
resumed in the permission mode it was started with: when the agent's files
show it ran with skip-permission ("yolo") flags, the command carries them.
Claude Code, Codex and Mistral Vibe record the mode; for other agents the
flags are left out.

- `--yolo` adds the flags to every command that supports them.
- `hopback config permission set yolo` does that on every run, and
  `set normal` leaves them out on every run. `set inherit` is the behavior
  described above, which is also what happens with nothing stored.

### `meta`

| Field | Meaning |
|---|---|
| `total` | Number of sessions that match the query and filters. |
| `offset` | The `--offset` that was applied (0 by default). |
| `limit` | The page size that was applied: `--limit`, the default of 50, or with `--all` the number of matches from the offset on. |
| `returned` | Length of `sessions`. |
| `next_offset` | The `--offset` for the next page, or `null` when there is none. |
| `state` | `more`: another page exists. `complete`: this page reaches the end (also for zero matches). `past_end`: the offset is at or beyond `total`, so the page is empty. |

To page, repeat the same command with `--offset <next_offset>` while `state` is
`more`; stop on `complete` or `past_end`.

## Order

With no query, sessions are newest first. With a query, they are ranked the way
the TUI ranks them. In both cases pinned sessions come first, as in the TUI.

## Flags

| Flag | Effect |
|---|---|
| `--limit <N>` | Page size, 1 or more. Default 50. |
| `--offset <N>` | Skip the first N matches. Default 0. |
| `--all` | Return every match from the offset on. Cannot be combined with `--limit`. |
| `--no-refresh` | Do not scan the agents' files first; answer from the index as it is. Fast, but may miss sessions created since the last run. |
| `--include-hidden` | Also return sessions hidden in hopback, with `"hidden": true`. |
| `--include-messages` | Add `messages`, the conversation text, to every session. |
| `--yolo` | Put skip-permission flags into every `resume_command` that supports them. |
| `--rebuild` | Rebuild the index first. The "Index rebuilt" line goes to stderr. |

`--limit`, `--offset`, `--all` and `--include-hidden` also work with `--list`.
Without `--json`, `--list` or `--no-tui` they are an error.

## Changing a session: `pin`, `unpin`, `rename`, `hide`, `unhide`

```bash
hb pin claude:5f0c2a9e-…
hb unpin 5f0c2a9e-…                # a bare id works when only one agent has it
hb rename codex:01a1cde9-… "Uploader retries"
hb rename codex:01a1cde9-… --reset # back to the agent's own title
hb hide claude:5f0c2a9e-…
hb unhide claude:5f0c2a9e-…
```

These change hopback's own metadata. Each prints one line such as `pinned claude:5f0c2a9e-…`. With `--json` it
prints the session as it is after the change instead:

```json
{ "schema_version": 1, "session": { "id": "…", "agent": "claude", "pinned": true, "…": "…" } }
```

`session` has the same fields as an entry of `sessions` above. An unknown
selector, or a bare id that two agents share, is an error (exit status 1,
nothing on stdout); the message for an ambiguous id lists the `agent:id` forms
to use. The index is refreshed first unless `--no-refresh` is given.

## Forking: `fork --json`

```bash
hb fork --json --to codex claude:5f0c2a9e-…   # translated fork, Claude Code to Codex
hb fork --json claude:5f0c2a9e-…              # native fork
hb fork --json --dry-run --to codex claude:5f0c2a9e-…
```

With `--json`, `fork` never starts an agent. It prints:

```json
{
  "schema_version": 1,
  "mode": "translated",
  "dry_run": false,
  "created": true,
  "session": {
    "id": "01a1d0aa-…",
    "agent": "codex",
    "title": "fix the login redirect",
    "original_title": "fix the login redirect",
    "directory": "/work/app",
    "timestamp": "2026-11-11T21:15:25Z",
    "message_count": 2,
    "pinned": false,
    "hidden": false,
    "forked_from": { "agent": "claude", "id": "5f0c2a9e-…" },
    "resume_command": ["codex", "resume", "01a1d0aa-…"]
  },
  "launch_command": ["codex", "resume", "01a1d0aa-…"],
  "warnings": []
}
```

| Field | Meaning |
|---|---|
| `mode` | `translated` (hopback wrote a new session for another agent) or `native` (the agent forks its own session). |
| `dry_run` | `--dry-run` was given: nothing was written or recorded. |
| `created` | The new session's files exist now. True for a translated fork without `--dry-run`. Always false for a native fork, which the agent creates when `launch_command` runs. |
| `session` | The new session, in the usual shape. `null` for a native fork whose agent picks the new id itself (Codex, OpenCode, Goose). For a native Claude Code fork hopback picks the id up front, so the session is described before it exists. |
| `launch_command` | The argv that opens the fork, to be run in `session.directory` (or the source session's directory when `session` is `null`). For a translated fork it equals `session.resume_command`. |
| `warnings` | Notes from the translation, for example that the new session could not be added to Codex's own session list. |

A translated fork holds the conversation as it was: no notice is added and
tool calls keep their names. The instructions an agent gives itself, such as
Codex's developer messages, are not part of the conversation and are left
out. The one addition is `[hopback]` in front of the session's title in the
target agent. Claude Code keeps that title; Codex replaces it with its own
once it has opened the session.

Translated forks exist between Claude Code and Codex. Any other pair, an
unknown selector, or an agent without a native fork is an error.

## Compatibility

`schema_version` changes only when a field is removed or changes meaning. New
fields may be added without a version bump, so ignore fields you do not know.
