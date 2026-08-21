//! Detects a JSON file's formatting style (indent unit, compact vs pretty)
//! and re-serializes subtrees to match it, so translated edits touch only
//! the bytes they must.

use serde::Serialize;
use serde_json::ser::PrettyFormatter;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Style {
    /// None = compact single-line file.
    pub indent: Option<String>,
}

impl Style {
    pub fn detect(raw: &str) -> Style {
        let indent = raw.lines().skip(1).find_map(|line| {
            let ws: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            (!ws.is_empty() && line.trim_start().len() < line.len()).then_some(ws)
        });
        Style { indent }
    }

    pub fn prefix(&self, depth: usize) -> String {
        match &self.indent {
            Some(unit) => unit.repeat(depth),
            None => String::new(),
        }
    }

    /// Separator between siblings at `depth`: `",\n" + indent` when pretty,
    /// `","` when compact.
    pub fn separator(&self, depth: usize) -> String {
        match &self.indent {
            Some(_) => format!(",\n{}", self.prefix(depth)),
            None => ",".to_string(),
        }
    }

    /// Serialize `value` as it would appear at `depth` (continuation lines
    /// indented to match).
    pub fn serialize(&self, value: &Value, depth: usize) -> Option<String> {
        match &self.indent {
            None => serde_json::to_string(value).ok(),
            Some(unit) => {
                let mut buf = Vec::new();
                let mut ser = serde_json::Serializer::with_formatter(
                    &mut buf,
                    PrettyFormatter::with_indent(unit.as_bytes()),
                );
                value.serialize(&mut ser).ok()?;
                let pretty = String::from_utf8(buf).ok()?;
                let prefix = self.prefix(depth);
                let mut lines = pretty.lines();
                let mut out = lines.next()?.to_string();
                for line in lines {
                    out.push('\n');
                    out.push_str(&prefix);
                    out.push_str(line);
                }
                Some(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_two_space_and_compact() {
        assert_eq!(
            Style::detect("{\n  \"a\": 1\n}").indent.as_deref(),
            Some("  ")
        );
        assert_eq!(
            Style::detect("{\n    \"a\": 1\n}").indent.as_deref(),
            Some("    ")
        );
        assert_eq!(Style::detect(r#"{"a":1}"#).indent, None);
    }

    #[test]
    fn test_serialize_nested_at_depth() {
        let style = Style {
            indent: Some("  ".into()),
        };
        let value: Value = serde_json::from_str(r#"{"x": [1, 2]}"#).unwrap();
        let out = style.serialize(&value, 1).unwrap();
        assert_eq!(out, "{\n    \"x\": [\n      1,\n      2\n    ]\n  }");
    }

    #[test]
    fn test_serialize_scalar_and_compact() {
        let pretty = Style {
            indent: Some("  ".into()),
        };
        assert_eq!(pretty.serialize(&Value::from(9090), 3).unwrap(), "9090");
        let compact = Style { indent: None };
        let value: Value = serde_json::from_str(r#"{"x": [1, 2]}"#).unwrap();
        assert_eq!(compact.serialize(&value, 1).unwrap(), r#"{"x":[1,2]}"#);
        assert_eq!(compact.separator(1), ",");
    }
}
