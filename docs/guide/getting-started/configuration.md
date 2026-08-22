---
title: Configuration
description: Customize RTK behavior via config.toml, environment variables, and per-project filters
sidebar:
  order: 4
---

# Configuration

## Config file location

| Platform | Path |
|----------|------|
| Linux | `~/.config/rtk/config.toml` |
| macOS | `~/Library/Application Support/rtk/config.toml` |

```bash
rtk config            # show current configuration
rtk config --create   # create config file with defaults
```

## Full config structure

```toml
[tracking]
enabled = true              # enable/disable token tracking
history_days = 90           # retention in days (auto-cleanup)
database_path = "/custom/path/history.db"   # optional override

[display]
colors = true               # colored output
emoji = true                # use emojis in output
max_width = 120             # maximum output width

[filters]
# These apply to file-reading commands (ls, find, grep, cat/rtk read).
# Paths matching these patterns are excluded from output, keeping noise low.
ignore_dirs = [".git", "node_modules", "target", "__pycache__", ".venv", "vendor"]
ignore_files = ["*.lock", "*.min.js", "*.min.css"]

[tee]
enabled = true              # save raw output on failure
mode = "failures"           # "failures" (default), "always", "never"
max_files = 20              # rotation: keep last N files
# directory = "/custom/tee/path"  # optional override

[telemetry]
enabled = true              # anonymous daily ping — see Telemetry & Privacy for full details

[hooks]
exclude_commands = []       # commands to never auto-rewrite

[posthook]
enabled = true              # PostToolUse output filtering (Claude Code)
exclude_paths = []          # globs vs file_path/url, e.g. ["**/*.min.js"]
tools = { read = true, grep = true, webfetch = true, websearch = true, glob = false, bash = true }

[posthook.formats]          # per-format converter: "auto" | "off"
json = "off"                # off by default: a converted view no file matches breaks Edit anchors
web = "auto"
lockfile = "auto"
term = "auto"               # Bash generic floor
```

For full details on what is collected, opt-out options, and GDPR rights, see [Telemetry & Privacy](../resources/telemetry.md).

## Environment variables

| Variable | Description |
|----------|-------------|
| `RTK_DISABLED=1` | Disable RTK for a single command (`RTK_DISABLED=1 git status`); also kills all posthook filtering (`RTK_DISABLED=1 claude`) |
| `RTK_POSTHOOK=0` | Disable PostToolUse output filtering only |
| `RTK_TEE=0` | Disable tee raw-output recovery (including posthook recall files) |
| `RTK_TEE_DIR` | Override the tee directory |
| `RTK_TELEMETRY_DISABLED=1` | Disable telemetry |
| `RTK_HOOK_AUDIT=1` | Enable hook audit logging |
| `SKIP_ENV_VALIDATION=1` | Skip env validation (useful with Next.js) |

## Tee system

When a command fails, RTK saves the full raw output to a local file and prints the path:

```
FAILED: 2/15 tests
[full output: ~/.local/share/rtk/tee/1707753600_cargo_test.log]
```

Your AI assistant can then read the file if it needs more detail, without re-running the command.

| Setting | Default | Description |
|---------|---------|-------------|
| `tee.enabled` | `true` | Enable/disable |
| `tee.mode` | `"failures"` | `"failures"`, `"always"`, `"never"` |
| `tee.max_files` | `20` | Rotation: keep last N files |
| Min size | 500 bytes | Outputs shorter than this are not saved |
| Max file size | 1 MB | Truncated above this |

## PostToolUse output filtering (posthook)

On Claude Code, RTK also compresses the output of the agent's native tools —
Read, Grep, WebFetch, WebSearch (Glob wired but off by default). The tool runs
raw; a PostToolUse hook sends the result through RTK, which replaces what the
model reads when (and only when) the filtered version is smaller:

- **Read of `.json` files** — off by default (`json = "auto"` to enable).
  Converting a Read result gives the agent a view no file on disk matches, so
  every `old_string` it copies from that view fails to match and the edit is
  lost. JSON compression instead goes through session TOON mirrors
  (`[toon] mirrors`), where the view *is* a file and anchors are real.
- **Grep (content mode)** — matches are grouped by file (`grep-group`),
  losslessly: every match is kept.
- **WebFetch / WebSearch** — ANSI stripping, HTML→Markdown when the response
  is raw HTML, TOON/minify when the response body is raw JSON, badge/comment
  slimming on markdown (link URLs kept).
- **Lockfiles** (package-lock.json, Cargo.lock, yarn.lock, pnpm-lock.yaml) —
  summarized to a `name@version` list + count (90%+ smaller; nobody edits a
  lockfile by hand, tooling regenerates it).
- **Base64 blobs** (embedded images, data URLs, fonts) — runs over ~1KB
  replaced with `[base64 image/png, 48 KB elided]` markers in JSON and web
  content; grep matches stay byte-faithful.
- **Bash generic floor** — commands NOT rewritten by RTK get objective
  byte-level cleanup: ANSI stripped, `\r` progress frames collapsed to the
  final frame, runs of ≥3 identical lines collapsed with an explicit `[xN]`
  marker, base64 blobs elided, and runaway outputs capped tail-biased
  (head 50 + last 200 lines + `[N lines elided]`) with the full copy in the
  recall file. rtk-prefixed commands are never double-processed, and reads
  of the recall directory pass through raw.

Native-tool chains are conversion-only: content is reformatted, never
truncated. The Bash floor's `cap` is the one deliberate exception, and the
recall file always holds the full output.

Before filtering, the raw output is saved to
`~/.local/share/rtk/tee/posthook/` (20-file rotation) and a
`[full output: …]` hint is appended, so the agent can always recover the
original via `cat`/`tail`.

Failed tool calls, `@`-referenced files, and Reads over the host's 256KB limit
never reach the hook and pass through unfiltered.

Toggles: `[posthook]` config section (per tool, per format, `exclude_paths`
globs), `RTK_POSTHOOK=0`, or `RTK_DISABLED=1` for everything. Savings appear
in `rtk gain` as `rtk posthook <tool> <format>` rows.

## Excluding commands from auto-rewrite

Prevent specific commands from being rewritten by the hook:

```toml
[hooks]
exclude_commands = ["git rebase", "git cherry-pick", "docker exec"]
```

Patterns match against the full command after stripping env prefixes (`sudo`, `VAR=val`), so `"psql"` excludes both `psql -h localhost` and `PGPASSWORD=x psql -h localhost`.

Subcommand patterns work too: `"git push"` excludes `git push origin main` but not `git status`.

Patterns starting with `^` are treated as regex:

```toml
[hooks]
exclude_commands = ["^curl", "^wget", "git rebase"]
```

Invalid regex patterns fall back to prefix matching.

Or for a single invocation:

```bash
RTK_DISABLED=1 git rebase main
```

## Telemetry

RTK sends one anonymous ping per day (23h interval). No personal data, no file paths, no command content.

Data sent: device hash, version, OS, architecture, command count/24h, top commands, savings %.

To opt out:

```bash
# Via environment variable
export RTK_TELEMETRY_DISABLED=1

# Via config.toml
[telemetry]
enabled = false
```

## Custom filters

Add your own filters (or override built-ins) in either location:

- **Project-local** — `.rtk/filters.toml` in your project root (committed with the repo)
- **User-global** — `~/.config/rtk/filters.toml` (applies to every project)

See [`src/filters/README.md`](https://github.com/rtk-ai/rtk/blob/master/src/filters/README.md) for the full TOML DSL reference.

### Trusting custom filters

Because a filter can rewrite what your AI assistant sees, custom filter files are **not applied until you trust them**. An untrusted (or edited) filter file is skipped silently on the command path. You review and manage trust with explicit commands:

```bash
rtk trust      # shows each filter and asks to confirm (--yes to skip the prompt)
rtk untrust    # revokes trust
```

`rtk init` also detects existing filters and lets you enable them — interactively, or non-interactively with `--trust-filters` / `--no-trust-filters`. Trust is tied to the file's contents (SHA-256), so editing a trusted file requires re-running `rtk trust`.

> **Upgrading:** earlier versions applied `~/.config/rtk/filters.toml` without trust. After upgrading, the user-global file is gated like project filters — if you already relied on a global filter, run `rtk trust` once to re-enable it.
