---
title: Scripts and agents
description: Use hopback from scripts and from coding agents, with JSON output and an agent skill.
---

```bash
hb --json --limit 10 "agent:codex api error"   # one JSON object on stdout
hb --json --include-messages --limit 1 "auth"   # with the conversation text
hb pin claude:5f0c2a9e-...                      # also: unpin, rename, hide, unhide
hb fork --json --to codex claude:5f0c2a9e-...   # the new session as JSON, nothing launched
hb --agent-context                              # instructions for a coding agent
```

## JSON output

`--json` has a versioned schema with paging, and every session carries the
exact `resume_command`. stdout carries exactly one JSON object, and notices go
to stderr. The [JSON output reference](../../reference/json/) has the schema.

## For coding agents

`hb --agent-context` prints an Agent Skill that tells a coding agent how to
search, page and resume safely.
[llms.txt](https://github.com/avgvi/hopback/blob/main/llms.txt) has the short
version.

## Without the list

```bash
hb .           # resume the newest session of this folder
hb . --print   # print the folder and the resume command instead of running it
hb --list      # list sessions and exit
```
