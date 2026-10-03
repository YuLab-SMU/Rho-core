//! Launcher authority and local caller identity. Requests cannot add scopes.
use crate::{NextHost, OperationError};
use rho_contract::{CallContext, CallerIdentity, CallerKind};

/// Scopes required by capabilities that the generic Core itself registers:
/// operations, plugin lifecycle, resources, generic drafts and Host paths.
/// Domain authority (R, environments, processes, remote compute, project
/// writes, ...) is never implied; a trusted launcher grants it explicitly.
pub const CORE_LOCAL_SCOPES: &[&str] = &[
    "operation.read",
    "project.references.read",
    "project.read",
    rho_plugins::PLUGINS_READ_SCOPE,
    rho_plugins::PLUGINS_WRITE_SCOPE,
    rho_plugins::PLUGINS_RUN_SCOPE,
    rho_plugins::RESOURCES_READ_SCOPE,
    rho_plugins::DOCUMENTS_READ_SCOPE,
    rho_plugins::DOCUMENTS_WRITE_SCOPE,
];

/// Additional scopes chosen by the trusted local launcher (for example
/// repeated `--grant-scope` arguments). Validated as ordinary scope tokens and
/// bounded by the call context's scope limit; they cannot be supplied by a
/// plugin manifest, a request body or a connected remote client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalGrants {
    scopes: std::collections::BTreeSet<String>,
}
impl LocalGrants {
    pub fn new<I, S>(scopes: I) -> Result<Self, OperationError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let grants = Self {
            scopes: scopes.into_iter().map(Into::into).collect(),
        };
        // Reuse the context's own token and count validation, including the
        // generic defaults, so a launcher cannot exceed the shared bound.
        NextHost::local_context_with(&grants)
            .validate()
            .map_err(|error| {
                OperationError::InvalidInput(format!("invalid --grant-scope: {error}"))
            })?;
        Ok(grants)
    }
    pub fn scopes(&self) -> &std::collections::BTreeSet<String> {
        &self.scopes
    }
}

impl NextHost {
    /// Context for a local, OS-user-owned CLI. Callers cannot put identity in
    /// Invocation. This grants only generic Core authority; domain scopes are
    /// added by the trusted launcher through [`NextHost::local_context_with`].
    pub fn local_context() -> CallContext {
        CallContext {
            view_scope: None,
            principal: None,
            caller: CallerIdentity {
                kind: CallerKind::Human,
                id: "local-user".into(),
            },
            scopes: CORE_LOCAL_SCOPES
                .iter()
                .map(|scope| (*scope).into())
                .collect(),
            connection_id: format!("cli:{}", std::process::id()),
            correlation_id: None,
            causation_id: None,
            trace_parent: None,
        }
    }

    /// Local context plus scopes explicitly selected by the trusted launcher.
    /// Plugin manifests and request bodies never reach this list.
    pub fn local_context_with(grants: &LocalGrants) -> CallContext {
        let mut context = Self::local_context();
        context.scopes.extend(grants.scopes.iter().cloned());
        context
    }
}
