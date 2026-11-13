//! Three-way, field-level merge (`docs/ARCHITECTURE.md` §9.4).
//!
//! Fields changed on one side only are taken from that side. A field changed on both sides to
//! different values keeps the local value (the edit about to be pushed) and is reported as a
//! [`FieldConflict`], so the losing value can be shown to the user.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Entity;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldConflict {
    /// Field name, or `"*"` when the whole entity was replaced.
    pub field: String,
    pub local: Value,
    pub remote: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MergeOutcome {
    pub merged: Entity,
    pub conflicts: Vec<FieldConflict>,
}

/// Merges a local edit with a concurrent remote edit of the same record.
pub fn merge(base: Option<&Entity>, local: &Entity, remote: &Entity) -> MergeOutcome {
    let (local_v, remote_v) = (to_value(local), to_value(remote));
    if local.kind() != remote.kind() {
        return MergeOutcome {
            merged: local.clone(),
            conflicts: vec![FieldConflict {
                field: "*".into(),
                local: local_v,
                remote: remote_v,
            }],
        };
    }
    let base_v = base.filter(|b| b.kind() == local.kind()).map(to_value);

    let empty = Map::new();
    let base_fields = base_v.as_ref().and_then(data).unwrap_or(&empty);
    let (Some(local_fields), Some(remote_fields)) = (data(&local_v), data(&remote_v)) else {
        // Unit-like data (should not happen with current entities): keep local.
        return MergeOutcome {
            merged: local.clone(),
            conflicts: Vec::new(),
        };
    };

    let mut merged = Map::new();
    let mut conflicts = Vec::new();
    let keys: std::collections::BTreeSet<&String> =
        local_fields.keys().chain(remote_fields.keys()).collect();
    for key in keys {
        let b = base_fields.get(key);
        let l = local_fields.get(key);
        let r = remote_fields.get(key);
        let pick = if l == r || r == b {
            l
        } else if l == b {
            r
        } else {
            conflicts.push(FieldConflict {
                field: key.clone(),
                local: l.cloned().unwrap_or(Value::Null),
                remote: r.cloned().unwrap_or(Value::Null),
            });
            l
        };
        if let Some(v) = pick {
            merged.insert(key.clone(), v.clone());
        }
    }

    let mut out = local_v.clone();
    out["data"] = Value::Object(merged);
    let merged = serde_json::from_value(out).unwrap_or_else(|_| local.clone());
    MergeOutcome { merged, conflicts }
}

fn to_value(e: &Entity) -> Value {
    serde_json::to_value(e).expect("entities always serialize")
}

fn data(v: &Value) -> Option<&Map<String, Value>> {
    v.get("data").and_then(Value::as_object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entity, Host, Snippet};

    fn host(label: &str, address: &str, port: Option<u16>) -> Entity {
        Entity::Host(Host {
            label: label.into(),
            address: address.into(),
            port,
            ..Default::default()
        })
    }

    #[test]
    fn disjoint_edits_are_combined() {
        let base = host("db", "10.0.0.1", None);
        let local = host("db-prod", "10.0.0.1", None);
        let remote = host("db", "10.0.0.1", Some(2222));
        let out = merge(Some(&base), &local, &remote);
        assert_eq!(out.merged, host("db-prod", "10.0.0.1", Some(2222)));
        assert!(out.conflicts.is_empty());
    }

    #[test]
    fn same_field_conflict_keeps_local_and_reports_remote() {
        let base = host("db", "10.0.0.1", None);
        let local = host("db", "10.0.0.2", None);
        let remote = host("db", "10.0.0.3", None);
        let out = merge(Some(&base), &local, &remote);
        assert_eq!(out.merged, local);
        assert_eq!(out.conflicts.len(), 1);
        assert_eq!(out.conflicts[0].field, "address");
        assert_eq!(out.conflicts[0].remote, "10.0.0.3");
    }

    #[test]
    fn identical_edits_do_not_conflict() {
        let base = host("db", "a", None);
        let both = host("db", "b", None);
        assert!(merge(Some(&base), &both, &both).conflicts.is_empty());
    }

    #[test]
    fn without_base_differences_are_conflicts() {
        let out = merge(None, &host("x", "a", None), &host("x", "b", None));
        assert_eq!(out.conflicts.len(), 1);
    }

    #[test]
    fn different_kinds_keep_local_whole() {
        let local = host("x", "a", None);
        let remote = Entity::Snippet(Snippet {
            label: "s".into(),
            ..Default::default()
        });
        let out = merge(None, &local, &remote);
        assert_eq!(out.merged, local);
        assert_eq!(out.conflicts[0].field, "*");
    }
}
