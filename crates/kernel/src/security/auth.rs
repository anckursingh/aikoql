//! Authorization Manager — RBAC + ACL policy evaluation.
//!
//! Owns the in-memory role-inheritance graph and per-type policy rules loaded
//! from persisted `aikoql:role` and `aikoql:policy` objects. The kernel
//! calls `AuthManager::authorize` before reads, writes, evolves, and deletes.

use crate::knowledge::kom::*;
use crate::storage::repository::KnowledgeRepository;
use crate::transaction::kernel::Subject;
use std::collections::{HashMap, HashSet};

pub const ROLE_TYPE: &str = "aikoql:role";
pub const POLICY_TYPE: &str = "aikoql:policy";

fn value_text(v: &Value) -> Option<&str> {
    match v {
        Value::Text(s) => Some(s),
        _ => None,
    }
}

fn text_prop(props: &PropertyMap, key: &str) -> Option<String> {
    props.get(key).and_then(value_text).map(String::from)
}

fn text_list(props: &PropertyMap, key: &str) -> Vec<String> {
    match props.get(key) {
        Some(Value::List(xs)) => xs.iter().filter_map(value_text).map(String::from).collect(),
        _ => Vec::new(),
    }
}

fn parse_acl_rules(props: &PropertyMap, key: &str) -> Vec<AclEntry> {
    let mut out = Vec::new();
    let Some(Value::List(xs)) = props.get(key) else {
        return out;
    };
    for x in xs {
        let Value::Map(m) = x else { continue };
        let principal = m.get("principal").and_then(value_text).map(String::from);
        let action = m
            .get("action")
            .and_then(value_text)
            .and_then(Action::from_name);
        let effect = m
            .get("effect")
            .and_then(value_text)
            .and_then(Effect::from_name);
        if let (Some(p), Some(a), Some(e)) = (principal, action, effect) {
            out.push(AclEntry {
                principal: p,
                action: a,
                effect: e,
            });
        }
    }
    out
}

fn access_denied(subject: &Subject, action: Action, koid: KOID) -> KError {
    KError::AccessDenied {
        subject: subject.name.clone(),
        action,
        koid,
    }
}

/// In-memory authorization cache.
#[derive(Clone, Debug, Default)]
pub struct AuthManager {
    role_parents: HashMap<String, Vec<String>>,
    policies: HashMap<String, Vec<AclEntry>>,
}

impl AuthManager {
    /// Load role and policy objects from the repository.
    pub fn load(repo: &KnowledgeRepository) -> KResult<Self> {
        let mut auth = AuthManager::default();
        for (koid, _version, ts, _state) in repo.scan_heads()? {
            let Some(ko) = repo.get_object_version(&koid, ts)? else {
                return Err(KError::Store("head points at missing version".into()));
            };
            if ko.metadata.type_name == ROLE_TYPE {
                if let Some(name) = text_prop(&ko.properties, "name") {
                    auth.role_parents
                        .insert(name, text_list(&ko.properties, "parents"));
                }
            } else if ko.metadata.type_name == POLICY_TYPE {
                if let Some(target) = text_prop(&ko.properties, "target_type") {
                    auth.policies
                        .insert(target, parse_acl_rules(&ko.properties, "rules"));
                }
            }
        }
        Ok(auth)
    }

    /// Reload from the repository, replacing the cached graph.
    pub fn refresh(&mut self, repo: &KnowledgeRepository) -> KResult<()> {
        *self = Self::load(repo)?;
        Ok(())
    }

    /// Decide whether `subject` may perform `action` on `ko`.
    pub fn authorize(
        &self,
        subject: &Subject,
        ko: &KnowledgeObject,
        action: Action,
    ) -> KResult<()> {
        // R9: tenant scope confinement. A tenant-scoped subject may only touch
        // objects in that tenant; untenanted objects are shared and stay
        // visible. Checked first so not even ownership or admin bypasses it —
        // an unscoped subject (tenant None) keeps the pre-R9 behavior.
        if let (Some(st), Some(kt)) = (&subject.tenant, &ko.metadata.tenant) {
            if st != kt {
                return Err(access_denied(subject, action, ko.koid));
            }
        }
        let sec = &ko.security;
        if subject.name == sec.owner || subject.is_admin() {
            return Ok(());
        }
        let principals = self.effective_principals(subject);
        if principals.contains("admin") {
            return Ok(());
        }
        if let Some(allowed) = Self::eval_acl(&sec.acl, &principals, action) {
            return if allowed {
                Ok(())
            } else {
                Err(access_denied(subject, action, ko.koid))
            };
        }
        if let Some(rules) = self.policies.get(&ko.metadata.type_name) {
            if let Some(allowed) = Self::eval_acl(rules, &principals, action) {
                return if allowed {
                    Ok(())
                } else {
                    Err(access_denied(subject, action, ko.koid))
                };
            }
        }
        Err(access_denied(subject, action, ko.koid))
    }

    fn effective_principals(&self, subject: &Subject) -> HashSet<String> {
        let mut principals = HashSet::new();
        principals.insert(subject.name.clone());
        principals.extend(self.effective_roles(subject));
        principals
    }

    fn effective_roles(&self, subject: &Subject) -> HashSet<String> {
        let mut roles = HashSet::new();
        let mut stack: Vec<String> = Vec::new();
        for r in &subject.roles {
            if roles.insert(r.clone()) {
                stack.push(r.clone());
            }
        }
        while let Some(r) = stack.pop() {
            if let Some(parents) = self.role_parents.get(&r) {
                for p in parents {
                    if roles.insert(p.clone()) {
                        stack.push(p.clone());
                    }
                }
            }
        }
        roles
    }

    fn eval_acl(
        entries: &[AclEntry],
        principals: &HashSet<String>,
        action: Action,
    ) -> Option<bool> {
        let mut allowed = false;
        for e in entries {
            if !principals.contains(&e.principal) {
                continue;
            }
            if e.action != action && e.action != Action::Admin {
                continue;
            }
            match e.effect {
                Effect::Deny => return Some(false),
                Effect::Allow => allowed = true,
            }
        }
        if allowed {
            Some(true)
        } else {
            None
        }
    }
}
