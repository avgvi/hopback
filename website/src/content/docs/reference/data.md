---
title: Your data
description: Where hopback keeps its index, and what it never touches.
---

- hopback keeps its index, pins, local titles and hidden flags in one SQLite
  file, `~/.cache/hopback/hopback.db` (or under `$XDG_CACHE_HOME`).
- It never edits, moves or deletes an agent's own session files. A translated
  fork writes a new session for the target agent and leaves the source alone.
- Nothing is sent anywhere. There is no network code.

## Coming from aiz

The first run copies your pins, local titles, hidden sessions and permission
setting from `~/.cache/aiz`.
