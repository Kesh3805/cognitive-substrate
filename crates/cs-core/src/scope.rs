//! Scope (where knowledge belongs) and access (who may see it).
//!
//! Visibility is *not* authorization. [`AccessContext`] is the authenticated
//! caller; every read and write is checked against it before data is touched.

use serde::{Deserialize, Serialize};

use crate::{AgentId, ConversationId, Error, ProjectId, Result, TenantId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Only the owning user (or, without an owner, the owning agent).
    Private,
    /// The owning agent and owner.
    Agent,
    /// Callers authorized for the project, and the owner.
    Project,
    /// Every caller in the tenant. Never crosses tenant boundaries.
    Tenant,
}

impl Visibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Agent => "agent",
            Self::Project => "project",
            Self::Tenant => "tenant",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "private" => Self::Private,
            "agent" => Self::Agent,
            "project" => Self::Project,
            "tenant" => Self::Tenant,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scope {
    pub tenant_id: TenantId,
    #[serde(default)]
    pub owner_user_id: Option<UserId>,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub conversation_id: Option<ConversationId>,
    pub visibility: Visibility,
}

impl Scope {
    /// A tenant-wide scope with no owner.
    pub fn tenant(tenant_id: TenantId) -> Self {
        Self {
            tenant_id,
            owner_user_id: None,
            agent_id: None,
            project_id: None,
            conversation_id: None,
            visibility: Visibility::Tenant,
        }
    }

    /// A private scope owned by `user`.
    pub fn private(tenant_id: TenantId, user: UserId) -> Self {
        Self {
            owner_user_id: Some(user),
            visibility: Visibility::Private,
            ..Self::tenant(tenant_id)
        }
    }

    pub fn with_project(mut self, project: ProjectId) -> Self {
        self.project_id = Some(project);
        self
    }

    pub fn with_agent(mut self, agent: AgentId) -> Self {
        self.agent_id = Some(agent);
        self
    }

    pub fn with_conversation(mut self, conversation: ConversationId) -> Self {
        self.conversation_id = Some(conversation);
        self
    }

    pub fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Structural validation: a visibility level must name the principal it
    /// refers to.
    pub fn validate(&self) -> Result<()> {
        match self.visibility {
            Visibility::Private if self.owner_user_id.is_none() && self.agent_id.is_none() => Err(
                Error::validation("private scope requires owner_user_id or agent_id"),
            ),
            Visibility::Agent if self.agent_id.is_none() => {
                Err(Error::validation("agent scope requires agent_id"))
            }
            Visibility::Project if self.project_id.is_none() => {
                Err(Error::validation("project scope requires project_id"))
            }
            _ => Ok(()),
        }
    }
}

/// Optional narrowing applied on top of authorization (never widening).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeFilter {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub conversation_id: Option<ConversationId>,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
}

impl ScopeFilter {
    pub fn matches(&self, scope: &Scope) -> bool {
        self.project_id.is_none_or(|p| scope.project_id == Some(p))
            && self
                .conversation_id
                .is_none_or(|c| scope.conversation_id == Some(c))
            && self.agent_id.is_none_or(|a| scope.agent_id == Some(a))
    }
}

/// The authenticated principal performing an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessContext {
    pub tenant_id: TenantId,
    #[serde(default)]
    pub user_id: Option<UserId>,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub project_ids: Vec<ProjectId>,
}

impl AccessContext {
    pub fn user(tenant_id: TenantId, user_id: UserId) -> Self {
        Self {
            tenant_id,
            user_id: Some(user_id),
            agent_id: None,
            project_ids: Vec::new(),
        }
    }

    pub fn agent(tenant_id: TenantId, agent_id: AgentId) -> Self {
        Self {
            tenant_id,
            user_id: None,
            agent_id: Some(agent_id),
            project_ids: Vec::new(),
        }
    }

    pub fn with_projects(mut self, projects: impl IntoIterator<Item = ProjectId>) -> Self {
        self.project_ids.extend(projects);
        self
    }

    fn is_owner(&self, scope: &Scope) -> bool {
        scope.owner_user_id.is_some() && scope.owner_user_id == self.user_id
    }

    fn is_agent(&self, scope: &Scope) -> bool {
        scope.agent_id.is_some() && scope.agent_id == self.agent_id
    }

    /// Read authorization. Storage adapters must implement exactly this rule
    /// in their query filters (see `docs/specifications/scope-and-access.md`).
    pub fn can_read(&self, scope: &Scope) -> bool {
        if scope.tenant_id != self.tenant_id {
            return false;
        }
        if self.is_owner(scope) {
            return true;
        }
        match scope.visibility {
            Visibility::Private => scope.owner_user_id.is_none() && self.is_agent(scope),
            Visibility::Agent => self.is_agent(scope),
            Visibility::Project => scope
                .project_id
                .is_some_and(|p| self.project_ids.contains(&p)),
            Visibility::Tenant => true,
        }
    }

    /// Write authorization: the caller may only write into scopes that name
    /// principals it actually is, and that it could subsequently read.
    pub fn can_write(&self, scope: &Scope) -> bool {
        scope.tenant_id == self.tenant_id
            && scope.owner_user_id.is_none_or(|u| self.user_id == Some(u))
            && scope.agent_id.is_none_or(|a| self.agent_id == Some(a))
            && scope
                .project_id
                .is_none_or(|p| self.project_ids.contains(&p))
            && self.can_read(scope)
    }

    pub fn ensure_read(&self, scope: &Scope) -> Result<()> {
        if self.can_read(scope) {
            Ok(())
        } else {
            Err(Error::forbidden("scope is not readable by caller"))
        }
    }

    pub fn ensure_write(&self, scope: &Scope) -> Result<()> {
        scope.validate()?;
        if self.can_write(scope) {
            Ok(())
        } else {
            Err(Error::forbidden("scope is not writable by caller"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (TenantId, UserId, UserId, ProjectId) {
        (
            TenantId::new_v7(),
            UserId::new_v7(),
            UserId::new_v7(),
            ProjectId::new_v7(),
        )
    }

    #[test]
    fn tenant_boundary_is_absolute() {
        let (t, u, _, _) = ids();
        let other = AccessContext::user(TenantId::new_v7(), u);
        assert!(!other.can_read(&Scope::tenant(t)));
    }

    #[test]
    fn private_is_owner_only() {
        let (t, alice, bob, _) = ids();
        let scope = Scope::private(t, alice);
        assert!(AccessContext::user(t, alice).can_read(&scope));
        assert!(!AccessContext::user(t, bob).can_read(&scope));
    }

    #[test]
    fn project_visibility_requires_membership() {
        let (t, alice, bob, p) = ids();
        let scope = Scope::private(t, alice)
            .with_project(p)
            .with_visibility(Visibility::Project);
        assert!(!AccessContext::user(t, bob).can_read(&scope));
        assert!(
            AccessContext::user(t, bob)
                .with_projects([p])
                .can_read(&scope)
        );
    }

    #[test]
    fn cannot_write_as_someone_else() {
        let (t, alice, bob, _) = ids();
        let scope = Scope::private(t, alice);
        assert!(!AccessContext::user(t, bob).can_write(&scope));
        assert!(AccessContext::user(t, alice).can_write(&scope));
    }

    #[test]
    fn visibility_must_name_its_principal() {
        let (t, _, _, _) = ids();
        assert!(
            Scope::tenant(t)
                .with_visibility(Visibility::Agent)
                .validate()
                .is_err()
        );
    }
}
