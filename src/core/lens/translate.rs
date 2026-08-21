//! Translates an edit expressed against a compressed view into the
//! equivalent raw-byte edit, with verify-before-emit.

use super::diff::{deep_diff, Change, ChangeKind};
use super::options::{parse_view, render, values_equal, ViewKind};
use super::spans::{self, Kind, Node, PathSeg};
use super::style::Style;
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub raw_old: String,
    pub raw_new: String,
    pub kind: ViewKind,
}

/// `None` = do not translate: the anchor is raw already, matches no view
/// uniquely, the edited view does not decode, or the result fails to verify.
pub fn translate_edit(raw: &str, view_old: &str, view_new: &str) -> Option<Translation> {
    if view_old.is_empty() || raw.contains(view_old) {
        return None;
    }
    let json: Value = serde_json::from_str(raw).ok()?;
    for kind in [ViewKind::Toon, ViewKind::Minified] {
        let Some(view) = render(&json, kind) else {
            continue;
        };
        if view.matches(view_old).count() != 1 {
            continue;
        }
        let edited = view.replacen(view_old, view_new, 1);
        let json_new = parse_view(&edited, kind)?;
        return translate_values(raw, &json, &json_new, kind);
    }
    None
}

fn translate_values(
    raw: &str,
    json: &Value,
    json_new: &Value,
    kind: ViewKind,
) -> Option<Translation> {
    let changes = deep_diff(json, json_new);
    if changes.is_empty() {
        return None;
    }
    let tree = spans::parse(raw)?;
    let style = Style::detect(raw);

    let mut ops: Vec<(Range<usize>, String)> = Vec::new();
    let mut inserts: BTreeMap<(usize, usize), (String, Vec<String>)> = BTreeMap::new();
    for change in &changes {
        match &change.kind {
            ChangeKind::Insert { value, after } => {
                let (range, base, sep, text) =
                    insert_parts(change, value, after.as_ref(), &tree, &style, raw)?;
                let entry = inserts
                    .entry((range.start, range.end))
                    .or_insert_with(|| (base, Vec::new()));
                entry.1.push(format!("{sep}{text}"));
            }
            _ => ops.push(op_for(change, &tree, &style, raw)?),
        }
    }
    for ((start, end), (base, texts)) in inserts {
        ops.push((start..end, format!("{base}{}", texts.concat())));
    }

    ops.sort_by_key(|(r, _)| (r.start, r.end));
    if ops.windows(2).any(|w| w[0].0.end > w[1].0.start) {
        return None;
    }
    let (raw_old, raw_new) = anchor_window(raw, &ops, &changes, &tree)?;

    let applied = raw.replacen(&raw_old, &raw_new, 1);
    let parsed: Value = serde_json::from_str(&applied).ok()?;
    if !values_equal(&parsed, json_new) || render(&parsed, kind)? != render(json_new, kind)? {
        return None;
    }

    Some(Translation {
        raw_old,
        raw_new,
        kind,
    })
}

/// Smallest raw window that covers every op AND occurs exactly once in the
/// file. The minimal span is tried first; when it repeats elsewhere (short
/// uniform records make this common) the window widens to the enclosing
/// value, then its ancestors. Widening only adds verbatim context — the
/// verify step still proves the result.
fn anchor_window(
    raw: &str,
    ops: &[(Range<usize>, String)],
    changes: &[Change],
    tree: &Node,
) -> Option<(String, String)> {
    let min_start = ops.first()?.0.start;
    let min_end = ops.last()?.0.end;

    let mut candidates: Vec<Range<usize>> = Vec::new();
    candidates.push(min_start..min_end);
    let mut prefix = common_prefix(changes);
    loop {
        if let Some(node) = tree.locate(&prefix) {
            let r = node.member_range.clone();
            if r.start <= min_start && r.end >= min_end && !candidates.contains(&r) {
                candidates.push(r);
            }
        }
        if prefix.pop().is_none() {
            break;
        }
    }

    for window in candidates {
        let Some(old) = raw.get(window.clone()) else {
            continue;
        };
        if old.is_empty() || raw.matches(old).count() != 1 {
            continue;
        }
        let mut new = String::new();
        let mut cursor = window.start;
        for (range, replacement) in ops {
            new.push_str(raw.get(cursor..range.start)?);
            new.push_str(replacement);
            cursor = range.end;
        }
        new.push_str(raw.get(cursor..window.end)?);
        return Some((old.to_string(), new));
    }
    None
}

/// Longest path prefix shared by every change (their deepest common parent
/// is at most this deep).
fn common_prefix(changes: &[Change]) -> Vec<PathSeg> {
    let first = &changes.first().map(|c| c.path.clone()).unwrap_or_default();
    let mut len = first.len();
    for change in changes.iter().skip(1) {
        len = len.min(
            first
                .iter()
                .zip(&change.path)
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    // A change's own last segment addresses the changed node itself; its
    // parent is the meaningful container to widen to.
    first[..len].to_vec()
}

fn op_for(
    change: &Change,
    tree: &Node,
    style: &Style,
    raw: &str,
) -> Option<(Range<usize>, String)> {
    match &change.kind {
        ChangeKind::Modify(value) => {
            let node = tree.locate(&change.path)?;
            Some((node.range.clone(), style.serialize(value, node.depth)?))
        }
        ChangeKind::Delete => {
            let (parent_path, last) = change.path.split_at(change.path.len().checked_sub(1)?);
            let parent = tree.locate(parent_path)?;
            let idx = child_index(parent, last.first()?)?;
            let child = &parent.children[idx];
            let range = if parent.children.len() == 1 {
                parent.range.start + 1..child.member_range.end
            } else if let Some(next) = parent.children.get(idx + 1) {
                child.member_range.start..next.member_range.start
            } else {
                parent.children[idx - 1].member_range.end..child.member_range.end
            };
            let _ = raw;
            Some((range, String::new()))
        }
        ChangeKind::Insert { .. } => None,
    }
}

/// Returns (anchor range, text kept at the anchor, separator, new member text).
fn insert_parts(
    change: &Change,
    value: &Value,
    after: Option<&PathSeg>,
    tree: &Node,
    style: &Style,
    raw: &str,
) -> Option<(Range<usize>, String, String, String)> {
    let (parent_path, last) = change.path.split_at(change.path.len().checked_sub(1)?);
    let parent = tree.locate(parent_path)?;
    if parent.kind == Kind::Scalar {
        return None;
    }
    let depth = parent.depth + 1;
    let body = style.serialize(value, depth)?;
    let text = match last.first()? {
        PathSeg::Key(key) => format!("{}: {}", serde_json::to_string(key).ok()?, body),
        PathSeg::Index(_) => body,
    };
    let sep = style.separator(depth);

    if let Some(seg) = after {
        let sib = &parent.children[child_index(parent, seg)?];
        let range = sib.member_range.clone();
        let base = raw.get(range.clone())?.to_string();
        return Some((range, base, sep, text));
    }
    match parent.children.first() {
        Some(first) => {
            let range = first.member_range.clone();
            let first_text = raw.get(range.clone())?.to_string();
            Some((
                range,
                String::new(),
                String::new(),
                format!("{text}{sep}{first_text}"),
            ))
        }
        None => {
            let range = parent.range.clone();
            let open = raw.get(range.start..range.start + 1)?;
            let close = raw.get(range.end - 1..range.end)?;
            let inner = match &style.indent {
                Some(_) => format!(
                    "\n{}{}\n{}",
                    style.prefix(depth),
                    text,
                    style.prefix(parent.depth)
                ),
                None => text,
            };
            Some((
                range,
                format!("{open}{inner}{close}"),
                String::new(),
                String::new(),
            ))
        }
    }
}

fn child_index(parent: &Node, seg: &PathSeg) -> Option<usize> {
    match seg {
        PathSeg::Key(k) => parent
            .children
            .iter()
            .position(|c| c.key.as_deref() == Some(k.as_str())),
        PathSeg::Index(i) => (*i < parent.children.len()).then_some(*i),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lens::options::encode_view;

    fn fixture_raw(name: &str) -> String {
        let raw = match name {
            "flat" => include_str!("../../../tests/fixtures/posthook/read_json_flat.json"),
            "nested" => include_str!("../../../tests/fixtures/posthook/read_json_nested.json"),
            _ => panic!("unknown fixture"),
        };
        let event: Value = serde_json::from_str(raw).unwrap();
        event["tool_response"]["file"]["content"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn toon_line(view: &str, needle: &str) -> String {
        view.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line containing {needle}"))
            .to_string()
    }

    fn apply(raw: &str, t: &Translation) -> Value {
        assert_eq!(raw.matches(&t.raw_old).count(), 1, "anchor unique in raw");
        serde_json::from_str(&raw.replacen(&t.raw_old, &t.raw_new, 1))
            .expect("valid JSON after edit")
    }

    fn changed_lines(a: &str, b: &str) -> usize {
        let la: Vec<&str> = a.lines().collect();
        let lb: Vec<&str> = b.lines().collect();
        la.iter().zip(&lb).filter(|(x, y)| x != y).count() + la.len().abs_diff(lb.len())
    }

    #[test]
    fn test_modify_tabular_row_in_flat_fixture() {
        let raw = fixture_raw("flat");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = toon_line(&view, "1,user_1,user1@example.com");
        let new = old.replace("user_1", "renamed_1");

        let t = translate_edit(&raw, &old, &new).expect("translates");
        assert_eq!(t.kind, ViewKind::Toon);
        let result = apply(&raw, &t);
        assert_eq!(result[1]["name"], "renamed_1");
        assert_eq!(result[0]["name"], "user_0");
        let after = raw.replacen(&t.raw_old, &t.raw_new, 1);
        assert_eq!(
            changed_lines(&raw, &after),
            1,
            "only the edited record's line changes"
        );
    }

    #[test]
    fn test_multi_field_row_edit_widens_to_unique_record() {
        let raw = fixture_raw("flat");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = toon_line(&view, "3,user_3,user3@example.com");
        // Two fields at once whose minimal window repeats across records.
        let new = old.replace(",false,", ",true,");
        assert_ne!(old, new);

        let t = translate_edit(&raw, &old, &new).expect("translates via widened window");
        let result = apply(&raw, &t);
        assert_eq!(result[3]["active"], true);
        assert_eq!(result[1]["active"], false, "neighbours untouched");
        assert_eq!(
            result[3]["name"], "user_3",
            "same record, other fields intact"
        );
    }

    #[test]
    fn test_modify_nested_leaf() {
        let raw = fixture_raw("nested");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = toon_line(&view, "port: 8080");
        let new = old.replace("8080", "9090");

        let t = translate_edit(&raw, &old, &new).expect("translates");
        assert_eq!(t.raw_old, "8080");
        assert_eq!(t.raw_new, "9090");
        let result = apply(&raw, &t);
        assert_eq!(result["config"]["server"]["port"], 9090);
    }

    #[test]
    fn test_insert_key_after_sibling() {
        let raw = fixture_raw("nested");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = toon_line(&view, "port: 8080");
        let indent: String = old.chars().take_while(|c| *c == ' ').collect();
        let new = format!("{old}\n{indent}timeout: 30");

        let t = translate_edit(&raw, &old, &new).expect("translates");
        let result = apply(&raw, &t);
        assert_eq!(result["config"]["server"]["timeout"], 30);
        let keys: Vec<&String> = result["config"]["server"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(
            keys,
            vec!["host", "port", "timeout", "tls"],
            "inserted right after port"
        );
    }

    #[test]
    fn test_delete_key() {
        let raw = fixture_raw("nested");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = format!("\n{}", toon_line(&view, "port: 8080"));

        let t = translate_edit(&raw, &old, "").expect("translates");
        let result = apply(&raw, &t);
        assert!(result["config"]["server"].get("port").is_none());
        assert_eq!(result["config"]["server"]["host"], "localhost");
    }

    #[test]
    fn test_minified_view_anchor() {
        let raw = fixture_raw("nested");
        let t = translate_edit(&raw, r#""port":8080"#, r#""port":1234"#).expect("translates");
        assert_eq!(t.kind, ViewKind::Minified);
        assert_eq!(apply(&raw, &t)["config"]["server"]["port"], 1234);
    }

    #[test]
    fn test_stale_array_count_aborts() {
        let raw = fixture_raw("flat");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let last = view.lines().last().unwrap().to_string();
        let new = format!("{last}\n  999,user_999,u999@example.com,true,1.5");
        assert!(
            translate_edit(&raw, &last, &new).is_none(),
            "[120] header now stale"
        );
    }

    #[test]
    fn test_insert_row_with_updated_header() {
        let raw = fixture_raw("flat");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let header = view.lines().next().unwrap().to_string();
        assert!(header.starts_with("[120]"));
        let new = format!(
            "{}\n  999,user_999,u999@example.com,true,1.5",
            header.replace("[120]", "[121]")
        );
        let t = translate_edit(&raw, &header, &new).expect("translates");
        let result = apply(&raw, &t);
        assert_eq!(result.as_array().unwrap().len(), 121);
        assert_eq!(result[0]["id"], 999);
        assert_eq!(result[1]["id"], 0);
    }

    #[test]
    fn test_unquoted_delimiter_slip_aborts() {
        let raw = fixture_raw("flat");
        let json: Value = serde_json::from_str(&raw).unwrap();
        let view = encode_view(&json).unwrap();
        let old = toon_line(&view, "1,user_1,user1@example.com");
        let new = old.replace("user_1", "Smith, Bob");
        assert!(translate_edit(&raw, &old, &new).is_none());
    }

    #[test]
    fn test_raw_anchor_and_unknown_anchor_not_translated() {
        let raw = fixture_raw("nested");
        assert!(translate_edit(&raw, "\"port\": 8080", "\"port\": 1").is_none());
        assert!(translate_edit(&raw, "no such text", "x").is_none());
        assert!(translate_edit(&raw, "", "x").is_none());
    }

    #[test]
    fn test_non_json_file_not_translated() {
        assert!(translate_edit("plain text", "text", "x").is_none());
    }
}
