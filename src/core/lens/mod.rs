//! JSON lens: compressed views of JSON files with transparent edit
//! translation back to raw bytes.
//!
//! Governing law: a converted view is shown only if it round-trips
//! (`decode(view)` value-equals the parsed file), and every edit made against
//! that view is translated to a raw-byte edit with verify-before-emit. Any
//! doubt at any step returns `None`; the caller passes the edit through
//! untouched and the host fails closed. Wrong writes are structurally
//! impossible — the worst outcome of a lens bug is a failed edit.
//!
//! Agent-agnostic: only content goes in and out. Agents are thin adapters
//! that marshal their edit payloads into `(raw, view_old, view_new)`.

pub mod diff;
pub mod mirror;
pub mod options;
pub mod spans;
pub mod style;
pub mod translate;
