//! Deep diff between two JSON values, as a list of path-addressed changes.
//! Containers of the same kind recurse; anything else is a whole-subtree
//! Modify. Insert carries the sibling it follows so raw placement preserves
//! the model's intended order.

use super::options::values_equal;
use super::spans::PathSeg;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum ChangeKind {
    Modify(Value),
    /// New member/element; `after` = preceding sibling present in the
    /// original (None = first position).
    Insert {
        value: Value,
        after: Option<PathSeg>,
    },
    Delete,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// Path to the changed node. For Insert, the last segment is the new
    /// key/index (not present in the original).
    pub path: Vec<PathSeg>,
    pub kind: ChangeKind,
}

pub fn deep_diff(a: &Value, b: &Value) -> Vec<Change> {
    let mut out = Vec::new();
    walk(a, b, &mut Vec::new(), &mut out);
    out
}

fn walk(a: &Value, b: &Value, path: &mut Vec<PathSeg>, out: &mut Vec<Change>) {
    match (a, b) {
        (Value::Object(oa), Value::Object(ob)) => {
            for key in oa.keys() {
                if !ob.contains_key(key) {
                    out.push(Change {
                        path: with(path, PathSeg::Key(key.clone())),
                        kind: ChangeKind::Delete,
                    });
                }
            }
            let mut last_shared: Option<PathSeg> = None;
            for (key, vb) in ob {
                match oa.get(key) {
                    Some(va) => {
                        if !values_equal(va, vb) {
                            path.push(PathSeg::Key(key.clone()));
                            walk(va, vb, path, out);
                            path.pop();
                        }
                        last_shared = Some(PathSeg::Key(key.clone()));
                    }
                    None => out.push(Change {
                        path: with(path, PathSeg::Key(key.clone())),
                        kind: ChangeKind::Insert {
                            value: vb.clone(),
                            after: last_shared.clone(),
                        },
                    }),
                }
            }
        }
        (Value::Array(xa), Value::Array(xb)) => {
            if let Some(change) = single_shift(xa, xb, path) {
                out.push(change);
                return;
            }
            let shared = xa.len().min(xb.len());
            for i in 0..shared {
                if !values_equal(&xa[i], &xb[i]) {
                    path.push(PathSeg::Index(i));
                    walk(&xa[i], &xb[i], path, out);
                    path.pop();
                }
            }
            for i in (shared..xa.len()).rev() {
                out.push(Change {
                    path: with(path, PathSeg::Index(i)),
                    kind: ChangeKind::Delete,
                });
            }
            for (i, value) in xb.iter().enumerate().skip(shared) {
                out.push(Change {
                    path: with(path, PathSeg::Index(i)),
                    kind: ChangeKind::Insert {
                        value: value.clone(),
                        after: i.checked_sub(1).map(PathSeg::Index),
                    },
                });
            }
        }
        _ => {
            if !values_equal(a, b) {
                out.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Modify(b.clone()),
                });
            }
        }
    }
}

/// One element inserted or deleted anywhere (the common row edit): the
/// remaining elements line up after a shift of one, so emit a single change
/// instead of index-wise modifies that would rewrite the whole array.
fn single_shift(xa: &[Value], xb: &[Value], path: &[PathSeg]) -> Option<Change> {
    let first_diff = xa
        .iter()
        .zip(xb)
        .position(|(a, b)| !values_equal(a, b))
        .unwrap_or(xa.len().min(xb.len()));
    if xb.len() == xa.len() + 1 {
        let aligned = xa[first_diff..]
            .iter()
            .zip(&xb[first_diff + 1..])
            .all(|(a, b)| values_equal(a, b));
        return aligned.then(|| Change {
            path: with(path, PathSeg::Index(first_diff)),
            kind: ChangeKind::Insert {
                value: xb[first_diff].clone(),
                after: first_diff.checked_sub(1).map(PathSeg::Index),
            },
        });
    }
    if xa.len() == xb.len() + 1 {
        let aligned = xa[first_diff + 1..]
            .iter()
            .zip(&xb[first_diff..])
            .all(|(a, b)| values_equal(a, b));
        return aligned.then(|| Change {
            path: with(path, PathSeg::Index(first_diff)),
            kind: ChangeKind::Delete,
        });
    }
    None
}

fn with(path: &[PathSeg], seg: PathSeg) -> Vec<PathSeg> {
    let mut p = path.to_vec();
    p.push(seg);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn test_modify_nested_leaf() {
        let changes = deep_diff(&v(r#"{"a":{"port":8080}}"#), &v(r#"{"a":{"port":9090}}"#));
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].path,
            vec![PathSeg::Key("a".into()), PathSeg::Key("port".into())]
        );
        assert_eq!(changes[0].kind, ChangeKind::Modify(v("9090")));
    }

    #[test]
    fn test_insert_key_after_sibling() {
        let changes = deep_diff(&v(r#"{"a":1,"c":3}"#), &v(r#"{"a":1,"b":2,"c":3}"#));
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].kind,
            ChangeKind::Insert {
                value: v("2"),
                after: Some(PathSeg::Key("a".into()))
            }
        );
    }

    #[test]
    fn test_delete_key_and_array_tail() {
        let changes = deep_diff(&v(r#"{"a":[1,2,3],"b":1}"#), &v(r#"{"a":[1,2]}"#));
        assert_eq!(changes.len(), 2);
        assert!(changes
            .iter()
            .any(|c| c.path == vec![PathSeg::Key("b".into())] && c.kind == ChangeKind::Delete));
        assert!(changes.iter().any(|c| c.path
            == vec![PathSeg::Key("a".into()), PathSeg::Index(2)]
            && c.kind == ChangeKind::Delete));
    }

    #[test]
    fn test_array_append() {
        let changes = deep_diff(&v("[{\"id\":1}]"), &v("[{\"id\":1},{\"id\":2}]"));
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].kind,
            ChangeKind::Insert {
                value: v("{\"id\":2}"),
                after: Some(PathSeg::Index(0))
            }
        );
    }

    #[test]
    fn test_single_insert_front_and_middle_is_one_change() {
        let changes = deep_diff(&v("[1,2,3]"), &v("[0,1,2,3]"));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, vec![PathSeg::Index(0)]);
        assert_eq!(
            changes[0].kind,
            ChangeKind::Insert {
                value: v("0"),
                after: None
            }
        );
        let changes = deep_diff(&v("[1,2,3]"), &v("[1,9,2,3]"));
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].kind,
            ChangeKind::Insert {
                value: v("9"),
                after: Some(PathSeg::Index(0))
            }
        );
    }

    #[test]
    fn test_single_delete_middle_is_one_change() {
        let changes = deep_diff(&v("[1,2,3]"), &v("[1,3]"));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, vec![PathSeg::Index(1)]);
        assert_eq!(changes[0].kind, ChangeKind::Delete);
    }

    #[test]
    fn test_numeric_tolerance_yields_no_change() {
        assert!(deep_diff(&v(r#"{"x":1500.0}"#), &v(r#"{"x":1500}"#)).is_empty());
    }
}
