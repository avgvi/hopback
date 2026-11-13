# Notes for agents and contributors

hopback (short command `hb`) is a Rust TUI and CLI that indexes the session
files coding agents keep on a machine, searches them, and resumes or forks the
chosen session. This file is for anyone changing the code, human or agent. To
*use* hopback from an agent, run `hb --agent-context` instead.

## Layout

| Path | What it holds |
|---|---|
| `src/main.rs`, `src/bin/hb.rs` | The two binaries. Both call `hopback::app::run`. |
| `src/app.rs` | Argument handling and the non-interactive commands. |
| `src/cli.rs` | The clap definition of every flag and subcommand. |
| `src/adapters/` | One file per agent, behind the `Adapter` trait in `mod.rs`; `registry.rs` lists them. |
| `src/index.rs` | Runs the adapters and writes sessions to the store. |
| `src/store/` | The SQLite store: sessions and FTS, overrides (pin, rename, hide), fork links, settings. |
| `src/search.rs`, `src/query.rs` | Ranking, and the `agent:` / `dir:` / date query keywords. |
| `src/title.rs` | Title cleaning, applied to every session by the indexer. |
| `src/quick.rs` | `hb .`: picking the session to continue for a folder. |
| `src/output.rs` | The `--json` schema. `docs/json.md` documents it. |
| `src/launcher.rs`, `src/bridge.rs` | Resume and native fork; translated forks between Claude Code and Codex. |
| `src/tui/` | `app.rs` (state, events), `keys.rs` (key map), `layout.rs`, `render.rs`, `theme.rs`, `format.rs`. |
| `src/agent.rs`, `skills/hopback/SKILL.md` | The agent skill, shell completions and the man page. |
| `tests/` | Integration tests; most run the real binary against a temp `HOME`. |
| `website/` | The docs site (Astro Starlight). `npm run dev` there previews it; `docs/json.md` and `CHANGELOG.md` are copied in at build time, and images come from `docs/`. |

## Checks

Every change must leave these three clean:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same commands on Linux and macOS.

## Rules

- **Never modify, move or delete an agent's own files.** Adapters only read.
  Open SQLite stores read-only. Translated forks write a *new* session for the
  target agent and leave the source alone.
- **Tests never touch real histories.** Build small anonymized fixtures in a
  temp dir and point `HOME` and `XDG_CACHE_HOME` at it. Fork tests run inside
  that temp `HOME` or with `--dry-run`.
- **Adapter contract.** The ids returned by `scan()` must be the ids of the
  sessions `parse()` returns, because the indexer deletes any indexed session
  whose id `scan()` no longer lists. `Session::mtime` is nanoseconds and must
  change when the session changes. Set `mtime_is_per_session()` to false when
  many sessions share one database file.
- **Titles** are cleaned in one place, `src/title.rs`. An adapter hands over
  the best title its agent stores and does not strip wrappers itself.
- **`--json` is a contract.** stdout carries exactly one JSON object; notices
  go to stderr. Adding a field is fine. Removing one or changing its meaning
  needs a new `schema_version`. Update `docs/json.md` in the same change.
- **Parsing changes** that should re-read sessions already in an index need a
  bump of `INDEX_FORMAT` in `src/index.rs`.
- **Keys** live in `src/tui/keys.rs`. Typing always goes to the search line,
  so a new action needs a modifier, or a place in the Ctrl+O menu. Keep
  `KEY_HELP` and the footer in step; tests check both.
- **Pop-ups** share one frame. Build a new one from `open_popup`,
  `popup_block`, `choice_line` and `hint_line` in `src/tui/render.rs`, and use
  colors from `THEME`, never the terminal's named colors.
- **The agent tab is search text.** Tab rewrites the `agent:` filter in the
  query (`query::set_agent`); there is no separate tab state to keep in step.
- **The man page is generated.** After changing flags or help text, run
  `cargo run --bin hopback -- man > man/hopback.1`. A test fails when the
  committed page is stale.

## Adding an agent

1. Add the variant to `AgentId` in `src/config.rs` (id, badge, color), keeping
   `AgentId::ALL` alphabetical.
2. Write `src/adapters/<agent>.rs` implementing `Adapter`, with a constructor
   that takes the root directory so tests can point it at a temp dir.
3. Register it in `src/adapters/registry.rs` and `src/adapters/mod.rs`.
4. Add `tests/<agent>_adapter.rs` with fixture tests for discovery, parsing,
   the rules that drop a session, and the resume command.
5. Add the agent to the README table and the changelog.

## Commits and releases

- Small commits, one logical step each, with a short imperative subject.
- Update `CHANGELOG.md` under Unreleased for anything a user would notice.
- Releases, tags, pushes and publishing to any registry are done by the
  maintainer. Do not run `cargo publish`, `npm publish`, `maturin publish`,
  `dist` publishing steps or `git push` as part of a change.
