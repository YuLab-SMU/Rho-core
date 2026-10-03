//! Writable Host assembly and explicit read-only observers. No domain selection.
use crate::ownership::ProjectLease;
use crate::{HostRuntime, NextHost, OperationError, QueryObserver, discovery, observer, paths};
use rho_operation::{
    CapabilityRegistry, OperationGateway, QueryGateway, SystemClock, UuidOperationIdGenerator,
};
use rho_sqlite::SqliteOperationJournal;
use std::{path::Path, sync::Arc};

impl NextHost {
    /// Open an ordinary project Host without selecting or starting a provider.
    pub async fn open_plugin_workspace(
        database: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
    ) -> Result<Self, OperationError> {
        let lease = ProjectLease::acquire(project_root.as_ref())?;
        Self::open_plugin_workspace_reserved(database.as_ref(), lease).await
    }

    pub(super) async fn open_plugin_workspace_reserved(
        database: &Path,
        lease: ProjectLease,
    ) -> Result<Self, OperationError> {
        let journal = Arc::new(SqliteOperationJournal::open(database)?);
        let mut protected = paths::protected_path_candidates(database);
        protected.push(lease.path().to_owned());
        let project_lease = Arc::new(lease);
        let project = project_lease.root().to_string_lossy().into_owned();
        let targets = vec![rho_contract::TargetRef {
            kind: "project".into(),
            identity: project.clone(),
        }];
        let discovery = discovery::DiscoveryOwner::new(Some(project.clone()), targets);
        let mut registry = CapabilityRegistry::new();
        for id in [
            "host.overview",
            "host.catalog",
            "host.describe",
            "host.core_contract",
        ] {
            registry.register_query(Arc::new(discovery::DiscoveryHandler::new(
                discovery.clone(),
                id,
            )))?;
        }
        let plugins = rho_plugins::PluginService::open(
            &rho_plugins::repository_path(database),
            project.clone(),
            protected,
            journal.clone(),
        )?;
        plugins.register(&mut registry)?;
        let event_port = observer::register_record_queries(
            &mut registry,
            journal.clone(),
            Some(project.clone()),
            true,
        )?;
        registry.validate_links()?;
        let registry = Arc::new(registry);
        discovery.bind(&registry);
        let gateway = Arc::new(
            OperationGateway::new(
                registry.clone(),
                journal,
                Arc::new(SystemClock),
                Arc::new(UuidOperationIdGenerator),
            )
            .with_project_scope(Some(project)),
        );
        event_port.bind(&gateway, &registry);
        plugins.bind(&registry, &gateway);
        let recovered_on_open = gateway.recover_incomplete().await?;
        let tasks = tokio_util::task::TaskTracker::new();
        let runtime = Arc::new(HostRuntime {
            gateway,
            queries: Arc::new(QueryGateway::new(registry.clone())),
            registry,
            plugins: Some(plugins.clone()),
            _project_lease: Some(project_lease),
        });
        plugins.bind_lifetime(&runtime, &tasks);
        Ok(Self {
            runtime,
            recovered_on_open,
            tasks,
        })
    }

    /// Compose standalone history observations without writer ownership or runtime startup.
    pub fn open_query_observer(
        database: impl AsRef<Path>,
        project: Option<&Path>,
    ) -> Result<QueryObserver, OperationError> {
        QueryObserver::open(database.as_ref(), project)
    }
    pub fn open_read_only(database: impl AsRef<Path>) -> Result<Self, OperationError> {
        let journal = Arc::new(SqliteOperationJournal::open_read_only(database)?);
        let observer = QueryObserver::from_sources(Some(journal), None)?;
        let registry = observer.registry;
        let gateway = observer
            .gateway
            .expect("A composed existing journal provides its read gateway");
        Ok(Self {
            runtime: Arc::new(HostRuntime {
                gateway,
                queries: Arc::new(QueryGateway::new(registry.clone())),
                registry,
                plugins: None,
                _project_lease: None,
            }),
            recovered_on_open: Vec::new(),
            tasks: tokio_util::task::TaskTracker::new(),
        })
    }
}
