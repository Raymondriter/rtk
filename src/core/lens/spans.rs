//! Span-tracking JSON parser: every value and key keeps its byte range in the
//! source so edits can be applied to raw bytes without re-serializing the
//! whole document. Hand-rolled recursive descent, string/escape aware.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Object,
    Array,
    Scalar,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    /// Value span (excluding a member's key).
    pub range: Range<usize>,
    /// For object members: decoded key and the key's span (with quotes).
    pub key: Option<String>,
    pub key_range: Option<Range<usize>>,
    /// Start of the member (key start) or element (value start) to value end.
    pub member_range: Range<usize>,
    pub depth: usize,
    pub children: Vec<Node>,
}

impl Node {
    pub fn locate(&self, path: &[PathSeg]) -> Option<&Node> {
        let Some((head, rest)) = path.split_first() else {
            return Some(self);
        };
        let child = match head {
            PathSeg::Key(k) => self
                .children
                .iter()
                .find(|c| c.key.as_deref() == Some(k.as_str())),
            PathSeg::Index(i) => self.children.get(*i),
        }?;
        child.locate(rest)
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

pub fn parse(raw: &str) -> Option<Node> {
    let mut p = Parser {
        s: raw.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let root = p.value(0)?;
    p.skip_ws();
    if p.pos != p.s.len() {
        return None;
    }
    Some(root)
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.s.len() && matches!(self.s[self.pos], b' ' | b'\t' | b'\n' | b'\r') {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn value(&mut self, depth: usize) -> Option<Node> {
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => {
                let start = self.pos;
                self.string()?;
                Some(scalar(start..self.pos, depth))
            }
            _ => {
                let start = self.pos;
                while self.pos < self.s.len()
                    && !matches!(
                        self.s[self.pos],
                        b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'
                    )
                {
                    self.pos += 1;
                }
                if self.pos == start {
                    return None;
                }
                Some(scalar(start..self.pos, depth))
            }
        }
    }

    /// Consumes a JSON string starting at the opening quote.
    fn string(&mut self) -> Option<()> {
        if self.peek()? != b'"' {
            return None;
        }
        self.pos += 1;
        while self.pos < self.s.len() {
            match self.s[self.pos] {
                b'\\' => self.pos += 2,
                b'"' => {
                    self.pos += 1;
                    return Some(());
                }
                _ => self.pos += 1,
            }
        }
        None
    }

    fn object(&mut self, depth: usize) -> Option<Node> {
        let start = self.pos;
        self.pos += 1;
        let mut children = Vec::new();
        self.skip_ws();
        if self.peek()? == b'}' {
            self.pos += 1;
            return Some(container(Kind::Object, start..self.pos, depth, children));
        }
        loop {
            self.skip_ws();
            let key_start = self.pos;
            self.string()?;
            let key_range = key_start..self.pos;
            let key: String =
                serde_json::from_str(std::str::from_utf8(&self.s[key_range.clone()]).ok()?).ok()?;
            self.skip_ws();
            if self.peek()? != b':' {
                return None;
            }
            self.pos += 1;
            self.skip_ws();
            let mut child = self.value(depth + 1)?;
            child.key = Some(key);
            child.key_range = Some(key_range);
            child.member_range = key_start..child.range.end;
            children.push(child);
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Some(container(Kind::Object, start..self.pos, depth, children));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Node> {
        let start = self.pos;
        self.pos += 1;
        let mut children = Vec::new();
        self.skip_ws();
        if self.peek()? == b']' {
            self.pos += 1;
            return Some(container(Kind::Array, start..self.pos, depth, children));
        }
        loop {
            self.skip_ws();
            let child = self.value(depth + 1)?;
            children.push(child);
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Some(container(Kind::Array, start..self.pos, depth, children));
                }
                _ => return None,
            }
        }
    }
}

fn scalar(range: Range<usize>, depth: usize) -> Node {
    Node {
        kind: Kind::Scalar,
        member_range: range.clone(),
        range,
        key: None,
        key_range: None,
        depth,
        children: Vec::new(),
    }
}

fn container(kind: Kind, range: Range<usize>, depth: usize, children: Vec<Node>) -> Node {
    Node {
        kind,
        member_range: range.clone(),
        range,
        key: None,
        key_range: None,
        depth,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spans_exact_on_nested_object() {
        let raw = "{\n  \"a\": {\"b\": [1, 2, {\"c\": \"x,y\"}]},\n  \"d\": true\n}";
        let root = parse(raw).expect("parses");
        assert_eq!(root.kind, Kind::Object);
        assert_eq!(root.range, 0..raw.len());

        let d = root.locate(&[PathSeg::Key("d".into())]).expect("d");
        assert_eq!(&raw[d.range.clone()], "true");
        assert_eq!(&raw[d.member_range.clone()], "\"d\": true");
        assert_eq!(d.depth, 1);

        let c = root
            .locate(&[
                PathSeg::Key("a".into()),
                PathSeg::Key("b".into()),
                PathSeg::Index(2),
                PathSeg::Key("c".into()),
            ])
            .expect("c");
        assert_eq!(&raw[c.range.clone()], "\"x,y\"");
        assert_eq!(c.depth, 4);
    }

    #[test]
    fn test_escaped_quotes_and_braces_in_strings() {
        let raw = r#"{"k": "a\"}{[\\", "n": -1.5e3}"#;
        let root = parse(raw).expect("parses");
        let k = root.locate(&[PathSeg::Key("k".into())]).unwrap();
        assert_eq!(&raw[k.range.clone()], r#""a\"}{[\\""#);
        let n = root.locate(&[PathSeg::Key("n".into())]).unwrap();
        assert_eq!(&raw[n.range.clone()], "-1.5e3");
    }

    #[test]
    fn test_real_flat_fixture_element_spans() {
        let event: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/posthook/read_json_flat.json"
        ))
        .unwrap();
        let raw = event["tool_response"]["file"]["content"].as_str().unwrap();
        let root = parse(raw).expect("parses");
        assert_eq!(root.kind, Kind::Array);
        assert_eq!(root.children.len(), 120);
        let second = &root.children[1];
        let text = &raw[second.range.clone()];
        assert!(text.starts_with('{') && text.ends_with('}'));
        assert!(text.contains("\"id\": 1"));
        let reparsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(reparsed["name"], "user_1");
    }

    #[test]
    fn test_rejects_trailing_garbage_and_malformed() {
        assert!(parse("{} x").is_none());
        assert!(parse("{\"a\": }").is_none());
        assert!(parse("[1, 2").is_none());
    }
}
