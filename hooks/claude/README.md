# Claude Code Hooks

> Part of [`hooks/`](../README.md) — see also [`src/hooks/`](../../src/hooks/README.md) for installation code

## Current integration (binary hooks)

`rtk init -g` registers the native binary command `rtk hook claude` in
`~/.claude/settings.json` under two events:

| Event | Matcher | Role |
|-------|---------|------|
| `PreToolUse` | `Bash` | Transparent command rewrite (`git status` → `rtk git status`) via `updatedInput` |
| `PostToolUse` | `Bash\|Read\|Grep\|Glob\|WebFetch\|WebSearch` | Output compression via `updatedToolOutput` (see `src/hooks/posthook.rs`); Bash = generic floor for commands not rtk-rewritten |

Both events run the same command; the binary dispatches on
`hook_event_name`. No script files, no `jq` dependency, fail-open on every
error (emit nothing, exit 0 — Claude Code keeps the original input/output).

- `rtk-awareness.md` is a slim 10-line instructions file embedded into CLAUDE.md by `rtk init`

## Legacy script hook (pre-binary migration)

- Shell-based `PreToolUse` hook (`rtk-rewrite.sh`) -- requires `jq` for JSON parsing
- Returns `updatedInput` JSON for transparent command rewrite (agent doesn't know RTK is involved)
- Exits silently (exit 0) on any failure: jq missing, rtk missing, rtk too old (< 0.23.0), no match
- Version guard checks `rtk --version` against minimum 0.23.0
- `rtk init -g` migrates script installs to the binary command and cleans up the old file

## Testing

```bash
# Run the full test suite for the legacy script hook (60+ assertions)
bash hooks/test-rtk-rewrite.sh

# Test against a specific hook path
HOOK=/path/to/rtk-rewrite.sh bash hooks/test-rtk-rewrite.sh

# Enable audit logging during testing
RTK_HOOK_AUDIT=1 RTK_AUDIT_DIR=/tmp bash hooks/test-rtk-rewrite.sh

# Binary hook smoke tests (no jq needed)
echo '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git status"}}' | rtk hook claude
rtk hook claude < tests/fixtures/posthook/read_json_flat.json
```
