//! Scope model — MRFC-0070 Phase A0.
//!
//! Scope defines the boundary within which a Knowledge Object applies.
//! Scopes nest: GLOBAL ⊃ ORGANIZATION ⊃ PROJECT ⊃ REPOSITORY ⊃ ... ⊃ SESSION.
//!
//! Extension key: `"scope"` — stored as the variant's snake_case name.

/// Twelve scope levels, from broadest to narrowest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Applies everywhere, across all organizations and systems.
    Global,
    /// Applies within an organization.
    Organization,
    /// Applies within a project.
    Project,
    /// Applies within a single repository.
    Repository,
    /// Applies within a specific branch.
    Branch,
    /// Applies within a directory tree.
    Directory,
    /// Applies within a single component/module.
    Component,
    /// Applies within a specific task/issue.
    Task,
    /// Applies within the current session.
    Session,
    /// Applies to a specific agent.
    Agent,
    /// Applies to a specific user.
    User,
    /// Applies only in a specific environment (dev/staging/prod).
    Environment,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Organization => "organization",
            Scope::Project => "project",
            Scope::Repository => "repository",
            Scope::Branch => "branch",
            Scope::Directory => "directory",
            Scope::Component => "component",
            Scope::Task => "task",
            Scope::Session => "session",
            Scope::Agent => "agent",
            Scope::User => "user",
            Scope::Environment => "environment",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Scope::Global),
            "organization" => Some(Scope::Organization),
            "project" => Some(Scope::Project),
            "repository" => Some(Scope::Repository),
            "branch" => Some(Scope::Branch),
            "directory" => Some(Scope::Directory),
            "component" => Some(Scope::Component),
            "task" => Some(Scope::Task),
            "session" => Some(Scope::Session),
            "agent" => Some(Scope::Agent),
            "user" => Some(Scope::User),
            "environment" => Some(Scope::Environment),
            _ => None,
        }
    }

    /// Numeric nesting level. Higher = broader.
    /// Global = 11, Environment = 0.
    pub fn level(self) -> u8 {
        match self {
            Scope::Global => 11,
            Scope::Organization => 10,
            Scope::Project => 9,
            Scope::Repository => 8,
            Scope::Branch => 7,
            Scope::Directory => 6,
            Scope::Component => 5,
            Scope::Task => 4,
            Scope::Session => 3,
            Scope::Agent => 2,
            Scope::User => 1,
            Scope::Environment => 0,
        }
    }
}

/// Deterministic scope nesting resolver.
///
/// `ScopeResolver::contains(outer, inner)` returns true when `inner` falls
/// within `outer`'s boundary. Example: Repository contains Directory, Branch,
/// Component, Task, Session, Agent, User, Environment.
pub struct ScopeResolver;

impl ScopeResolver {
    /// Returns true if `outer` is an ancestor of (or equal to) `inner`.
    pub fn contains(outer: Scope, inner: Scope) -> bool {
        outer.level() >= inner.level()
    }

    /// Most specific scope that contains both (higher level = broader).
    pub fn least_common_ancestor(a: Scope, b: Scope) -> Scope {
        if a.level() >= b.level() {
            a
        } else {
            b
        }
    }

    /// Find the narrowest scope from a set that contains `target`.
    pub fn resolve(target: Scope, candidates: &[Scope]) -> Option<Scope> {
        candidates
            .iter()
            .filter(|s| Self::contains(**s, target))
            .min_by_key(|s| s.level())
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_contains_everything() {
        assert!(ScopeResolver::contains(Scope::Global, Scope::Repository));
        assert!(ScopeResolver::contains(Scope::Global, Scope::Session));
        assert!(ScopeResolver::contains(Scope::Global, Scope::Environment));
    }

    #[test]
    fn repository_contains_component() {
        assert!(ScopeResolver::contains(Scope::Repository, Scope::Component));
        assert!(ScopeResolver::contains(Scope::Repository, Scope::Task));
        assert!(!ScopeResolver::contains(Scope::Component, Scope::Repository));
    }

    #[test]
    fn least_common_ancestor_finds_broader() {
        assert_eq!(
            ScopeResolver::least_common_ancestor(Scope::Component, Scope::Task),
            Scope::Component
        );
        assert_eq!(
            ScopeResolver::least_common_ancestor(Scope::Repository, Scope::Directory),
            Scope::Repository
        );
    }

    #[test]
    fn resolve_finds_narrowest_match() {
        let candidates = vec![Scope::Global, Scope::Repository, Scope::Component];
        let found = ScopeResolver::resolve(Scope::Task, &candidates);
        assert_eq!(found, Some(Scope::Component)); // narrowest that contains Task
    }

    #[test]
    fn scope_round_trip() {
        for s in [Scope::Global, Scope::Repository, Scope::Session] {
            let txt = s.as_str();
            let back = Scope::from_str(txt);
            assert_eq!(back, Some(s));
        }
    }
}
