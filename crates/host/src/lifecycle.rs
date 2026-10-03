//! Ending a Host waits for its accepted tasks and preserves uncertain outcomes.
use crate::{NextHost, OperationError};

impl NextHost {
    /// Hosting lifecycle only: keep accepted work alive after an edge disconnects.
    pub fn is_idle(&self) -> bool {
        self.tasks.is_empty()
            && !self
                .runtime
                .gateway
                .commit_recovery()
                .has_retained_results()
    }

    /// The caller must first stop accepting new work through every edge.
    pub async fn prepare_workbench_quit(&self) -> Result<(), OperationError> {
        if !self.is_idle() {
            return Err(OperationError::Unavailable(
                "Accepted work or an uncommitted result remains; inspect and reconcile its original operation before quitting"
                    .into(),
            ));
        }
        Ok(())
    }

    /// The caller must first stop accepting new work through every edge.
    pub async fn drain(&self) {
        self.tasks.close();
        self.tasks.wait().await;
        if let Some(plugins) = &self.runtime.plugins {
            plugins.suspend_for_restart().await;
        }
    }
}
