#![forbid(unsafe_code)]
mod authority;
mod config;
mod discovery;
mod lifecycle;
mod observer;
mod ownership;
mod paths;
mod plugin_views;
mod port_contracts;
mod ports;
mod workspace;

pub use authority::{CORE_LOCAL_SCOPES, LocalGrants};
pub use config::{HostProfile, ReservedHost};
pub use observer::QueryObserver;
pub use paths::default_database;
pub use rho_operation::OperationError;
pub use rho_sqlite::ApplicationStore;

use ownership::ProjectLease;
use rho_contract::{CallContext, CapabilityDescriptor, OperationRecord};
use rho_operation::{CapabilityRegistry, OperationGateway, QueryGateway};
use std::sync::Arc;

pub struct NextHost {
    runtime: Arc<HostRuntime>,
    recovered_on_open: Vec<OperationRecord>,
    tasks: tokio_util::task::TaskTracker,
}

// Accepted tasks retain this entire lifetime, not just a gateway or query
// handle. Drop adapters/journal before releasing the project's OS lease.
struct HostRuntime {
    registry: Arc<CapabilityRegistry>,
    gateway: Arc<OperationGateway>,
    queries: Arc<QueryGateway>,
    plugins: Option<Arc<rho_plugins::PluginService>>,
    _project_lease: Option<Arc<ProjectLease>>,
}

impl NextHost {
    pub fn capability_publications(&self) -> tokio::sync::watch::Receiver<u64> {
        self.runtime.registry.subscribe_publications()
    }

    pub fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        self.refresh_plugin_registrations();
        self.runtime.gateway.registry_descriptors()
    }
    /// Shared discovery/tool visibility; target-specific authority is checked
    /// again by its owner when a control is admitted.
    pub fn capabilities_for(&self, context: &CallContext) -> Vec<CapabilityDescriptor> {
        port_contracts::visible(self.capabilities(), context)
    }

    pub fn recovered_on_open(&self) -> &[OperationRecord] {
        &self.recovered_on_open
    }

    fn refresh_plugin_registrations(&self) {
        if let Some(plugins) = &self.runtime.plugins {
            // Native failure can withdraw capabilities; this observes only state
            // already held by the owner, and never starts or repairs a process.
            if let Err(error) = plugins.refresh() {
                eprintln!("plugin registration refresh: {error}");
            }
        }
    }
}
