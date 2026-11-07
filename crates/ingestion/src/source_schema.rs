//! Source-schema identity: a stable fingerprint of a `ConnectorMetadata` and a
//! structural diff between two snapshots of the same source.

use crate::connector_bridge::{ConnectorMetadata, ContainerInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContainerDiff {
    pub name: String,
    pub added_fields: Vec<String>,
    pub removed_fields: Vec<String>,
    /// (field, old type, new type)
    pub type_changes: Vec<(String, String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SchemaDiff {
    pub added_containers: Vec<String>,
    pub removed_containers: Vec<String>,
    pub changed: Vec<ContainerDiff>,
}

impl SchemaDiff {
    pub fn is_empty(&self) -> bool {
        self.added_containers.is_empty()
            && self.removed_containers.is_empty()
            && self.changed.is_empty()
    }
}

impl ConnectorMetadata {
    /// SHA-256 hex of the schema's identity: names, types, keys, nullability,
    /// references and observed type sets; never row counts, sample sizes or ratios.
    pub fn fingerprint(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for c in &self.containers {
            let mut uniques: Vec<String> = c
                .unique_groups
                .iter()
                .map(|g| {
                    let mut g = g.clone();
                    g.sort();
                    g.join(",")
                })
                .collect();
            uniques.sort();
            lines.push(format!(
                "C|{}|{}|{}",
                c.schema.as_deref().unwrap_or(""),
                c.name,
                uniques.join(";")
            ));
            for f in &c.fields {
                let mut observed = f
                    .profile
                    .as_ref()
                    .map(|p| p.types.clone())
                    .unwrap_or_default();
                observed.sort();
                lines.push(format!(
                    "F|{}|{}|{}|{}|{}|{}|{:?}|{}",
                    c.name,
                    f.name,
                    f.data_type,
                    f.is_primary_key,
                    f.nullable,
                    f.is_unique,
                    f.provenance,
                    observed.join(",")
                ));
            }
        }
        for r in &self.references {
            lines.push(format!(
                "R|{}|{}|{}|{}|{}",
                r.from_container,
                r.from_fields.join(","),
                r.to_container,
                r.to_fields.join(","),
                r.name.as_deref().unwrap_or("")
            ));
        }
        lines.sort();
        let mut h = Sha256::new();
        h.update(self.connector_type.as_bytes());
        h.update([u8::from(self.truncated)]);
        for l in &lines {
            h.update(l.as_bytes());
            h.update(b"\n");
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// What changed between two snapshots of one source, by container and field name.
pub fn diff_metadata(old: &ConnectorMetadata, new: &ConnectorMetadata) -> SchemaDiff {
    let names = |m: &ConnectorMetadata| -> BTreeSet<String> {
        m.containers.iter().map(|c| c.name.clone()).collect()
    };
    let (o, n) = (names(old), names(new));
    let mut diff = SchemaDiff {
        added_containers: n.difference(&o).cloned().collect(),
        removed_containers: o.difference(&n).cloned().collect(),
        changed: Vec::new(),
    };
    for nc in &new.containers {
        let Some(oc) = old.containers.iter().find(|c| c.name == nc.name) else {
            continue;
        };
        let types = |c: &ContainerInfo| -> BTreeMap<String, String> {
            c.fields
                .iter()
                .map(|f| (f.name.clone(), f.data_type.clone()))
                .collect()
        };
        let (ot, nt) = (types(oc), types(nc));
        let cd = ContainerDiff {
            name: nc.name.clone(),
            added_fields: nt
                .keys()
                .filter(|k| !ot.contains_key(*k))
                .cloned()
                .collect(),
            removed_fields: ot
                .keys()
                .filter(|k| !nt.contains_key(*k))
                .cloned()
                .collect(),
            type_changes: nt
                .iter()
                .filter_map(|(k, v)| match ot.get(k) {
                    Some(old_t) if old_t != v => Some((k.clone(), old_t.clone(), v.clone())),
                    _ => None,
                })
                .collect(),
        };
        if !(cd.added_fields.is_empty()
            && cd.removed_fields.is_empty()
            && cd.type_changes.is_empty())
        {
            diff.changed.push(cd);
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector_bridge::{
        ContainerInfo, FieldInfo, FieldProfile, Provenance, ReferenceInfo,
    };

    fn field(name: &str, ty: &str) -> FieldInfo {
        FieldInfo {
            name: name.into(),
            data_type: ty.into(),
            nullable: false,
            ..Default::default()
        }
    }

    fn meta() -> ConnectorMetadata {
        ConnectorMetadata {
            connector_type: "postgres".into(),
            label: "shop".into(),
            containers: vec![
                ContainerInfo {
                    name: "customers".into(),
                    fields: vec![field("id", "int"), field("email", "text")],
                    row_count: Some(10),
                    ..Default::default()
                },
                ContainerInfo {
                    name: "orders".into(),
                    fields: vec![field("id", "int"), field("customer_id", "int")],
                    row_count: Some(99),
                    ..Default::default()
                },
            ],
            references: vec![ReferenceInfo {
                from_container: "orders".into(),
                from_fields: vec!["customer_id".into()],
                to_container: "customers".into(),
                to_fields: vec!["id".into()],
                name: Some("orders_customer_fk".into()),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn old_json_without_the_new_keys_still_deserialises() {
        let j = r#"{"connector_type":"postgres","label":"x","containers":[{"name":"t","fields":[{"name":"id","data_type":"int","is_primary_key":true,"nullable":false,"is_unique":true}],"row_count":null}],"references":[],"version":null}"#;
        let m: ConnectorMetadata = serde_json::from_str(j).unwrap();
        assert_eq!(m.containers[0].fields[0].provenance, Provenance::Declared);
        assert!(m.containers[0].unique_groups.is_empty() && !m.truncated);
    }

    #[test]
    fn fingerprint_is_sha256_hex_and_ignores_row_counts_and_order() {
        let a = meta();
        let mut b = meta();
        b.containers.reverse();
        b.containers[0].fields.reverse();
        b.containers[0].row_count = Some(123_456);
        assert_eq!(a.fingerprint().len(), 64);
        assert!(a.fingerprint().chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_changes_on_new_column_type_change_or_nullability() {
        let base = meta().fingerprint();
        let mut added = meta();
        added.containers[0].fields.push(field("phone", "text"));
        let mut retyped = meta();
        retyped.containers[0].fields[1].data_type = "varchar".into();
        let mut nullable = meta();
        nullable.containers[0].fields[1].nullable = true;
        for (what, m) in [
            ("added", added),
            ("retyped", retyped),
            ("nullable", nullable),
        ] {
            assert_ne!(m.fingerprint(), base, "{what}");
        }
    }

    #[test]
    fn observed_ratios_do_not_change_the_fingerprint_but_observed_types_do() {
        let profile = |types: &[&str], presence: f64| FieldProfile {
            types: types.iter().map(|t| t.to_string()).collect(),
            presence_ratio: presence,
            null_ratio: 0.0,
            samples: 100,
        };
        let with = |p: FieldProfile| {
            let mut m = meta();
            m.containers[0].fields[1].provenance = Provenance::Observed;
            m.containers[0].fields[1].profile = Some(p);
            m.fingerprint()
        };
        assert_eq!(with(profile(&["Int"], 0.9)), with(profile(&["Int"], 0.4)));
        assert_ne!(
            with(profile(&["Int"], 0.9)),
            with(profile(&["Int", "Null"], 0.9))
        );
    }

    #[test]
    fn diff_reports_added_removed_and_retyped_fields() {
        let old = meta();
        let mut new = meta();
        new.containers[0].fields.push(field("phone", "text"));
        new.containers[0].fields.retain(|f| f.name != "email");
        new.containers[1].fields[1].data_type = "bigint".into();
        new.containers.push(ContainerInfo {
            name: "refunds".into(),
            ..Default::default()
        });
        new.containers.retain(|c| c.name != "nothing");
        let d = diff_metadata(&old, &new);
        assert_eq!(d.added_containers, ["refunds"]);
        assert!(d.removed_containers.is_empty());
        let customers = d.changed.iter().find(|c| c.name == "customers").unwrap();
        assert_eq!(customers.added_fields, ["phone"]);
        assert_eq!(customers.removed_fields, ["email"]);
        let orders = d.changed.iter().find(|c| c.name == "orders").unwrap();
        assert_eq!(
            orders.type_changes,
            [(
                "customer_id".to_string(),
                "int".to_string(),
                "bigint".to_string()
            )]
        );
    }

    #[test]
    fn diff_reports_removed_containers_and_identical_is_empty() {
        let old = meta();
        let mut new = meta();
        assert!(diff_metadata(&old, &new).is_empty());
        new.containers.retain(|c| c.name != "orders");
        assert_eq!(diff_metadata(&old, &new).removed_containers, ["orders"]);
    }
}
