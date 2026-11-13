---
title: Supported agents
description: The fifteen agents hopback indexes, how each is resumed, and which can be forked.
---

| Agent | Tab | Resumes with | Native fork | Translated fork |
|---|---|---|---|---|
| Claude Code | `claude` | `claude --resume <id>` | yes | to and from Codex |
| Codex | `codex` | `codex resume <id>` | yes | to and from Claude Code |
| Antigravity | `agy` | `agy --conversation <id>` | no | no |
| OpenCode | `opencode` | `opencode <dir> --session <id>` | yes | no |
| Kimi Code | `kimi` | `kimi --session <id>` | no | no |
| Kiro CLI | `kiro` | `kiro-cli chat --resume-id <id>` | no | no |
| Goose | `goose` | `goose session --resume --session-id <id>` | yes | no |
| Pi | `pi` | `pi --session <id>` | no | no |
| Cursor CLI | `cursor` | `agent --resume <id>` | no | no |
| Grok Build | `grok` | `grok --resume <id>` | no | no |
| Gemini CLI | `gemini` | `gemini --resume <id>` | no | no |
| GitHub Copilot CLI | `copilot` | `copilot --resume <id>` | no | no |
| Copilot in VS Code | `vscode` | `code <dir>` | no | no |
| Crush | `crush` | `crush --session <id>` | no | no |
| Mistral Vibe | `vibe` | `vibe --resume <id>` | no | no |

## What is listed

hopback only reads the agents' files. Subagent sessions and sessions with no
prompt are left out.

## Goose on macOS

The Goose desktop app on macOS does not put `goose` on `PATH`. When it is
missing there, hopback runs the copy inside `Goose.app`.
