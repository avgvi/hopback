# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The npm package is `hopback-tui`. npm refused the name `hopback` as too
  similar to another package. The commands it installs are still `hopback`
  and `hb`.

## [0.2.0] - 2026-11-13

The relaunch of aiz as hopback.

### Added

- `hb`, a short command installed next to `hopback`. Both are the same program.
- First-run import of aiz pins, local renames, hidden sessions and the
  permission setting. The old `~/.cache/aiz` data is left in place.
- Packaging for every install channel: cargo-dist (release archives for
  macOS and Linux on arm64 and x86_64 with checksums, a shell installer, a
  Homebrew formula for `avgvi/homebrew-tap`, an npm installer and the release
  workflow), a `pyproject.toml` for maturin wheels, and a Nix flake. Every
  channel installs both `hopback` and `hb`.
- `--agent-context` prints a bundled Agent Skill (`skills/hopback/SKILL.md`)
  that tells a coding agent how to search, page, resume and fork with `hb`.
- `hopback completions bash|zsh|fish` (also elvish and powershell) and
  `hopback man`; the generated man page is in `man/hopback.1`.
- `AGENTS.md` for contributors and `llms.txt`.
- `hopback pin`, `unpin`, `rename`, `hide` and `unhide <selector>`, each with
  `--json` to print the session after the change.
- `hopback fork --json` prints the new session, its `resume_command` and the
  `launch_command` instead of starting the agent.
- Search-line editing: ← → Home End, `Ctrl+A`, `Ctrl+E`, `Ctrl+W`, `Ctrl+U`
  and Delete, with a real cursor.
- Mouse support: click selects, double-click resumes, the wheel scrolls the
  list or the preview under the pointer.
- Adapters for Antigravity (`agy`), Kimi Code, Pi, Cursor CLI, Grok Build and
  Gemini CLI, each with its own id, badge color and resume command.
- `--json`: the matching sessions as one JSON object on stdout, with a
  versioned schema (`schema_version: 1`), local metadata (`pinned`, `hidden`,
  `original_title`, `forked_from`) and the exact `resume_command`. Paging with
  `--limit`, `--offset` and `--all`. See `docs/json.md`.
- Quick continue: `hb .` resumes the most recently used session of the current
  folder, whatever the agent, or of the closest parent folder that has one.
  `--agent` narrows it; with no session it exits 1 with a clear message.
- `--print` prints the folder and the resume command instead of running it.
- `--no-refresh` to answer from the existing index, and `--include-hidden`.
- CI on Linux and macOS: `cargo fmt --check`, `cargo clippy` with warnings
  denied, and `cargo test`.
- Kiro CLI sessions are indexed, in both layouts it keeps under
  `~/.kiro/sessions`, and resume with `kiro-cli chat --resume-id <id>`.
- Goose sessions are indexed from its SQLite store and resume with
  `goose session --resume --session-id <id>`; `Ctrl+F` forks one with Goose's
  own `--fork`. When the Goose desktop app is installed but `goose` is not on
  `PATH`, hopback runs the copy inside the app.
- `Ctrl+T` pins or unpins the selected session, `Ctrl+R` or `F2` renames it,
  and `Ctrl+X` hides it, straight from the list. The footer shows the pin and
  rename keys.
- Filter keywords in the search line are colored: the keyword, an agent's name
  in that agent's color, other values in green, and a value hopback does not
  know in red and struck through.
- While an agent or date value is being typed, the rest is suggested after the
  cursor. `Tab` or `→` takes it, and the list already shows the suggested
  filter.
- Right-click on a session opens the actions menu for it.
- The rename box has a cursor and the same editing keys as the search line.
- `--json --include-messages` adds `messages`, the prompts and replies of each
  session, to the output.
- hopback reads the permission mode Claude Code records in a session, so a
  session that ran with skipped permissions is known as one.

### Changed

- Long titles in the table are cut with `…` instead of being clipped, and a
  translated fork's title is cleaned like any other.
- The README is rewritten around the install commands, `hb .`, translated
  forks, the supported agents and the keys.
- New keys. Typing always goes to the search; `Ctrl+O` opens an actions menu
  for the selected session (`f` fork, `p` pin, `r` rename, `h` hide, `y`
  copy); `Ctrl+P` now toggles the preview; `F1` lists every key; `Esc` clears
  the search and quits only when it is already empty.
- Removed ``Ctrl+` `` and the direct `Ctrl+P` pin, `Ctrl+R` rename and
  `Ctrl+X` hide bindings. Pin, rename and hide are in the `Ctrl+O` menu.
- New TUI layout: a status line with the refresh summary and "shown /
  indexed" counts, a framed search box with a syntax hint, agent tabs with
  session counts in each agent's color, a table with Agent, Title, Directory,
  Turns and Age columns plus pin (★) and fork (↳) marks, a preview to the
  right of the table, and a footer with each key in a chip. Narrow terminals
  drop the Directory column first, then the preview.
- The preview shows the session's title, folder and local date above the
  conversation, and colors the user's prompts apart from the replies.
- Removed the Ctrl+Up / Ctrl+Down preview resize; the preview now takes a
  fixed share of the width.
- The index is rebuilt once after an upgrade that changes how sessions are
  read. Pins, renames and hidden flags are kept.
- An unknown `--agent` value is now an error instead of being ignored.

- The project is renamed from aiz to hopback: crate, binaries, help text and
  the store location (`~/.cache/hopback/hopback.db`).
- `Tab` writes the agent into the search line as `agent:<name>` instead of
  keeping a separate, invisible tab filter. A typed `agent:` filter fills in
  the matching tabs, and `--agent` on the command line shows up there too.
- Every pop-up (actions, fork, rename, hide, help) is drawn on one frame with
  the same selection bar and hint line.
- Resuming and forking no longer ask about permissions. A session starts in
  the mode it was started with: with the agent's skip-permission flags when
  it ran that way, without them otherwise. `--yolo` and
  `hb config permission set normal|yolo|inherit` still override that.
- A translated fork holds the conversation as it was. It no longer starts
  with a notice telling the new agent to treat the transcript as untrusted
  history, tool calls keep their names, and Codex's instructions are not
  replaced. The one addition is `[hopback]` in front of the title the target
  agent shows.
- A fork into Codex is listed with the sandbox and approval settings of your
  latest Codex session in that folder (or anywhere, when the folder has
  none) instead of being forced to read-only. It is recorded for the model
  provider Codex is configured with, where it used to say `openai`, which
  made Codex refuse to open it on any other provider.
- A fork into Codex shows the earlier replies in Codex's transcript, not
  only the prompts.
- A fork into Claude Code leaves out the instructions Codex gave itself,
  which used to appear as prompts you had typed.
- A Codex session counts as started with skipped permissions only when it
  ran with approvals off and no sandbox. Runs with approvals off inside a
  sandbox, which is what `codex exec` does, resume normally.
- The agent skill and `llms.txt` no longer tell agents to ask for approval
  before resuming, forking, pinning or renaming.
- The actions menu lists each action with the key that runs it directly, in
  the order pin, rename, fork, copy, hide. Its single-letter keys are gone.
- Key chips and hint text use fixed colors instead of the terminal's own
  grays, which were too light or too dim depending on the terminal theme.
- The footer drops chips on a narrow terminal instead of running off the
  edge, and no longer lists `Tab` and `Ctrl+P` (they are in the `F1` help).
- A footer message such as "pinned" clears on the next key press.

### Fixed

- Codex sessions written by current Codex builds are indexed. Prompts are now
  read from `response_item` messages, the session id comes from the rollout
  file name, and subagent rollouts (reviews, guardian checks, spawned threads)
  are skipped, as `codex resume` does.
- Titles are readable: paste wrappers (`<pasted_content …>`), slash-command
  envelopes, injected context blocks, image labels and leading slash commands
  are stripped, and the title falls back to the first meaningful line a user
  wrote. Titles are one line, at most 80 characters.
- Claude Code sessions use the name Claude Code itself shows: the `/rename`
  sidecar, then `custom-title` records, the sessions index and `ai-title`
  records, before the first prompt.
- Codex sessions are titled from the prompt as typed, not from the model input
  with its image labels and context blocks.
- OpenCode sessions are found again: the adapter reads the `session_v2` and
  `session_message` tables of current OpenCode as well as the older schema,
  and skips subagent child sessions.
- A Codex thread rename in `session_index.jsonl` updates the indexed title.
- Saving an empty name in the rename box brings the original title back at
  once, not only after a restart.
- A half-typed `agent:` value no longer empties the list.

## [0.1.0] - 2026-06-19

### Added

- Initial release as aiz: a TUI that indexes Claude Code, Codex, Copilot CLI,
  Copilot in VS Code, Crush, OpenCode and Vibe sessions, searches them with
  SQLite FTS5 and fuzzy matching, and resumes or forks the selected session.
- Pins, local renames and hidden sessions, kept in hopback's own store.
- Native forks, and translated forks between Claude Code and Codex.

[Unreleased]: https://github.com/avgvi/hopback/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/avgvi/hopback/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/avgvi/hopback/releases/tag/v0.1.0
