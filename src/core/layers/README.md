# core/layers — Output-Compression Layer Engine

Agent-agnostic compression layers applied to tool outputs by the PostToolUse
hook (`src/hooks/posthook.rs`). This module sees only content + a
`ContentFormat`; everything that knows the words "claude", "Read", or
"tool_response" lives on the hooks side.

## Contract

- `Layer::apply(input, ctx) -> Continue(String) | ShortCircuit(String)`
- `run_chain(layers, input, ctx)` is **pure**: no I/O, no panic handling.
  `catch_unwind` and the `never_worse` size guard are the caller's job.
- Every layer is fail-open: on parse failure or any internal error it returns
  the input unchanged (`Continue`), never an error.

## Layer names (frozen public vocabulary)

`ansi`, `toon`, `minify-json`, `web-md`, `truncate`, `grep-group`.
Used in tracking rows and future config values — never rename.
Reserved for Part 2: `dedup`, `unicode`, `ipynb-strip`, `tree-sitter`.

## Part 1 chains (hardcoded, RTK-owned, conversion-only)

Chains reformat content, they never drop it — no truncation on the posthook
path. `truncate` exists as a reserved layer only.

| Format | Chain | Notes |
|--------|-------|-------|
| `json` | `[toon]` | Shape-gated: uniform array of flat objects → TOON; else minified JSON; parse failure → passthrough |
| `web` | `[ansi, web-md, toon]` | `web-md` skipped when content is already markdown-ish (tag-density sniff on first 1 KB); `toon` fires only when the body parses as JSON (raw API responses) |
| `matches` | `[ansi, grep-group]` | Lossless grouping (all matches emitted); passes through unchanged when any line is not `path:line:content` (context lines stay faithful) |

## Reuse map

| Layer | Wraps |
|-------|-------|
| `ansi` | `utils::strip_ansi` |
| `truncate` | `utils::truncate` (per-line cap) + `filter::smart_truncate` (windowing) |
| `minify-json` | `serde_json` parse → compact re-serialization (lossless) |
| `toon` | `toon-format` crate (default-features = false) |
| `web-md` | `htmd` crate |
| `grep-group` | seeded from `pipe_cmd::grep_wrapper`; converge with `search.rs::group_matches` in Part 2 |

`toml_filter::apply_filter` is intentionally untouched — its semantics are
locked by the 63 built-in filters.
