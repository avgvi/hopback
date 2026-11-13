#!/bin/sh
# Builds a throwaway HOME with made-up Claude Code, Codex, Gemini CLI and Pi
# sessions, so the demo can be recorded without showing anyone's real history.
#
#   sh docs/demo/setup.sh [dir]      # default: /tmp/hopback-demo
#
# Then run hopback against it:
#
#   HOME=/tmp/hopback-demo/home XDG_CACHE_HOME=/tmp/hopback-demo/cache \
#     PATH=/tmp/hopback-demo/bin:$PATH hb
#
# The `claude` and `codex` commands in <dir>/bin are stand-ins that only print
# what they were asked to do. Remove <dir>/bin from PATH to record with the
# real agents instead.
set -eu

DEMO=${1:-/tmp/hopback-demo}
HOME_DIR=$DEMO/home
rm -rf "$DEMO"
WORK=$HOME_DIR/code
mkdir -p "$HOME_DIR" "$DEMO/cache" "$DEMO/bin" "$WORK/shop-api" "$WORK/shop-web" "$WORK/infra"

claude_session() {
  # id, project folder, timestamp, prompt, reply, second prompt, second reply
  dir="$HOME_DIR/.claude/projects/$(printf '%s' "$2" | tr '/.' '--')"
  mkdir -p "$dir"
  cat > "$dir/$1.jsonl" <<JSON
{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"$3","sessionId":"$1","cwd":"$2","message":{"role":"user","content":"$4"}}
{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"$3","sessionId":"$1","cwd":"$2","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"text","text":"$5"}]}}
{"type":"user","uuid":"u2","parentUuid":"a1","timestamp":"$3","sessionId":"$1","cwd":"$2","message":{"role":"user","content":"$6"}}
{"type":"assistant","uuid":"a2","parentUuid":"u2","timestamp":"$3","sessionId":"$1","cwd":"$2","message":{"id":"m2","role":"assistant","model":"claude","content":[{"type":"text","text":"$7"}]}}
JSON
}

codex_session() {
  # id, project folder, timestamp, prompt, reply
  day=$(printf '%s' "$3" | cut -c1-10 | tr '-' '/')
  dir="$HOME_DIR/.codex/sessions/$day"
  mkdir -p "$dir"
  cat > "$dir/rollout-$(printf '%s' "$3" | cut -c1-19 | tr ':' '-')-$1.jsonl" <<JSON
{"type":"session_meta","payload":{"id":"$1","timestamp":"$3","cwd":"$2","source":"cli"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"$4"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"$5"}]}}
JSON
}

gemini_session() {
  # id, project folder, timestamp, prompt, reply
  project="$HOME_DIR/.gemini/tmp/$(basename "$2")"
  mkdir -p "$project/chats"
  printf '%s' "$2" > "$project/.project_root"
  stamp=$(printf '%s' "$3" | cut -c1-16 | tr ':' '-')
  cat > "$project/chats/session-$stamp-$(printf '%s' "$1" | cut -c1-8).jsonl" <<JSON
{"sessionId":"$1","projectHash":"0000000000000000000000000000000000000000000000000000000000000000","startTime":"$3","lastUpdated":"$3"}
{"id":"m1","timestamp":"$3","type":"user","content":[{"text":"$4"}]}
{"id":"m2","timestamp":"$3","type":"gemini","content":"$5","thoughts":[],"model":"gemini"}
JSON
}

pi_session() {
  # id, project folder, timestamp, prompt, reply
  dir="$HOME_DIR/.pi/agent/sessions/--$(printf '%s' "${2#/}" | tr '/' '-')--"
  mkdir -p "$dir"
  cat > "$dir/$(printf '%s' "$3" | tr ':' '-')_$1.jsonl" <<JSON
{"type":"session","version":3,"id":"$1","timestamp":"$3","cwd":"$2"}
{"type":"message","id":"a1","parentId":null,"timestamp":"$3","message":{"role":"user","content":"$4"}}
{"type":"message","id":"a2","parentId":"a1","timestamp":"$3","message":{"role":"assistant","content":[{"type":"text","text":"$5"}]}}
JSON
}

now=$(date -u +%s)
ago() { # seconds ago -> ISO time, on macOS and Linux
  date -u -r $((now - $1)) +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d "@$((now - $1))" +%Y-%m-%dT%H:%M:%SZ
}

claude_session 5f0c2a9e-1111-4aaa-8bbb-000000000001 "$WORK/shop-api" "$(ago 900)" \
  "The checkout endpoint returns 500 when the cart has a discount code. Find out why." \
  "The discount is applied twice: once in CartService.total and again in the tax step, which makes the amount negative for 100% codes." \
  "Fix it and add a regression test." \
  "Done. CartService.total is now the only place that applies discounts, and test_checkout_full_discount covers the 100% case."
claude_session 5f0c2a9e-2222-4aaa-8bbb-000000000002 "$WORK/shop-web" "$(ago 7200)" \
  "Migrate the product page from the old carousel to the new image gallery component." \
  "The gallery needs width and height for each image to avoid layout shift. I will read them from the asset manifest." \
  "Keep the keyboard navigation working." \
  "Arrow keys and Home/End move between images, and focus stays on the gallery after a swap."
claude_session 5f0c2a9e-3333-4aaa-8bbb-000000000003 "$WORK/infra" "$(ago 93600)" \
  "Why did last night's database backup job take four hours?" \
  "The job ran while the reindex was holding locks. Moving the backup to 03:00 avoids the overlap." \
  "Change the schedule and document it." \
  "Schedule updated in backups.tf and noted in the runbook."
claude_session 5f0c2a9e-4444-4aaa-8bbb-000000000004 "$WORK/shop-api" "$(ago 432000)" \
  "Add rate limiting to the public search API." \
  "A token bucket per API key, 60 requests a minute, with a Retry-After header on 429." \
  "Make the limit configurable." \
  "The limit comes from SEARCH_RATE_LIMIT and defaults to 60."
codex_session 01a1cde9-b5f4-7141-b2c9-000000000001 "$WORK/shop-api" "$(ago 3600)" \
  "Profile the order export and make it faster." \
  "The export loaded every order line one by one. Batching the query cut it from 41 s to 3 s."
codex_session 01a1cde9-b5f4-7141-b2c9-000000000002 "$WORK/shop-web" "$(ago 259200)" \
  "Set up visual regression tests for the checkout flow." \
  "Playwright snapshots for cart, address and payment steps are in tests/visual."
claude_session 5f0c2a9e-5555-4aaa-8bbb-000000000005 "$WORK/infra" "$(ago 777600)" \
  "Rotate the staging database credentials and update the secrets." \
  "New credentials are in the secret store, and the api and worker deployments now read them from there." \
  "Check that nothing still uses the old ones." \
  "No references left: the last one was a cron job, which now mounts the same secret."
codex_session 01a1cde9-b5f4-7141-b2c9-000000000003 "$WORK/shop-api" "$(ago 172800)" \
  "Upgrade the payment SDK to v5 and fix what breaks." \
  "Three call sites used the removed charge.create. They now go through PaymentIntent, and the webhook handler accepts the new event names."
codex_session 01a1cde9-b5f4-7141-b2c9-000000000004 "$WORK/infra" "$(ago 1036800)" \
  "Why is the nightly CI run flaky on the search tests?" \
  "The tests share one index and run in parallel. Giving each worker its own index name made twenty runs in a row pass."
gemini_session 7c1e4d2a-9a10-4c3e-8f21-000000000001 "$WORK/shop-web" "$(ago 14400)" \
  "Write the release notes for storefront 4.2 from the merged pull requests." \
  "Drafted in docs/releases/4.2.md: the new image gallery, faster search suggestions, and the discount code fix at checkout."
gemini_session 7c1e4d2a-9a10-4c3e-8f21-000000000002 "$WORK/shop-api" "$(ago 345600)" \
  "Explain how the rules engine picks between stacked discount codes." \
  "It sorts the codes by priority, applies percentage discounts before fixed amounts, and stops at the first code marked exclusive."
pi_session 3b9d6f10-4e2c-4b7a-a1d5-000000000001 "$WORK/infra" "$(ago 25200)" \
  "Add a health check to the worker container and wire it into the deploy script." \
  "The worker answers on /healthz, and deploy.sh waits for three healthy checks before it shifts traffic."
pi_session 3b9d6f10-4e2c-4b7a-a1d5-000000000002 "$WORK/shop-web" "$(ago 518400)" \
  "Make the cart drawer usable with the keyboard." \
  "Focus moves into the drawer when it opens, Esc closes it, and Tab stays inside until it is closed."

# Codex lists its sessions in a SQLite file. With an empty one in place a
# translated fork registers there and hopback hands over to `codex`, as it
# does on a machine that has run Codex.
if command -v sqlite3 >/dev/null 2>&1; then
  sqlite3 "$HOME_DIR/.codex/state_5.sqlite" "CREATE TABLE threads (
    id TEXT PRIMARY KEY, rollout_path TEXT, created_at INTEGER, updated_at INTEGER,
    source TEXT, model_provider TEXT, cwd TEXT, title TEXT, sandbox_policy TEXT,
    approval_mode TEXT, tokens_used INTEGER, has_user_event INTEGER, archived INTEGER,
    cli_version TEXT, first_user_message TEXT, memory_mode TEXT, model TEXT,
    created_at_ms INTEGER, updated_at_ms INTEGER);"
fi

for agent in claude codex; do
  cat > "$DEMO/bin/$agent" <<SH
#!/bin/sh
# Stand-in for the demo: shows what hopback started.
home=\$(cd "\$HOME" && pwd -P)
dir=\$(pwd -P)
case \$dir in "\$home"*) dir="~\${dir#"\$home"}" ;; esac
printf '\n  %s (demo stand-in) started in %s\n  arguments: %s\n\n' "$agent" "\$dir" "\$*"
SH
  chmod +x "$DEMO/bin/$agent"
done

echo "demo home ready in $DEMO"
