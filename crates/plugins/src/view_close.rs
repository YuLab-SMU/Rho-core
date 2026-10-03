//! Close cooperation belongs to the view owner, independent of any UI toolkit.
use crate::{PluginService, service::invalid};
use rho_contract as host;
use rho_operation::OperationError;
use rho_plugin_protocol::*;
use std::{collections::BTreeSet, time::Duration};

pub(crate) struct CloseAttempt {
    pub(crate) operation: OperationId,
    prepared: BTreeSet<RequestId>,
    refused: Option<String>,
}
impl CloseAttempt {
    pub(crate) fn renderer_ended(&mut self) {
        self.refused = Some("a participating document ended during close preparation".into());
    }
    pub(crate) fn sealed(&self, participants: usize) -> bool {
        self.refused.is_none() && participants > 0 && self.prepared.len() == participants
    }
}

/// Interrupted preparation keeps the connection open. It never closes the
/// native session or establishes that owner content was saved.
struct Preparation<'a> {
    service: &'a PluginService,
    view: ViewInstanceId,
    operation: OperationId,
}
impl Drop for Preparation<'_> {
    fn drop(&mut self) {
        if let Some(live) = self.service.views.lock().unwrap().get_mut(&self.view)
            && live
                .closing
                .as_ref()
                .is_some_and(|close| close.operation == self.operation)
        {
            live.closing = None;
        }
        self.service
            .view_sequences
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

impl PluginService {
    pub(crate) fn prepare_view_close(
        &self,
        context: &host::CallContext,
        args: &ClosePluginView,
    ) -> Result<PluginViewRecord, OperationError> {
        let record = self.view_record(context, &args.view)?;
        self.check_window_context(context, &record.window)?;
        if record.closed {
            return Ok(record);
        }
        let views = self.views.lock().unwrap();
        if views
            .get(&args.view)
            .is_some_and(|live| live.closing.is_some())
        {
            return Err(OperationError::ContentChanged(
                "another close is already preparing this view".into(),
            ));
        }
        match &args.mode {
            PluginViewCloseMode::Cooperate => {
                if !views
                    .get(&args.view)
                    .is_some_and(|live| !live.renderers.is_empty())
                {
                    return Err(invalid(
                        "no participant is connected; use an explicit disconnect with the observed connection identity",
                    ));
                }
            }
            PluginViewCloseMode::Disconnect { connection } => {
                if views
                    .get(&args.view)
                    .map(|live| &live.connection.connection)
                    != connection.as_ref()
                {
                    return Err(OperationError::ContentChanged(
                        "the observed view connection changed".into(),
                    ));
                }
            }
        }
        Ok(record)
    }

    /// Only the containing scoped channel can register or answer for its own
    /// view. Each document registers separately; one document cannot attest that
    /// another participant completed its owner-defined preparation.
    pub fn cooperate_with_view_close(
        &self,
        context: &host::CallContext,
        view: &ViewInstanceId,
        request: &PluginViewRequest,
    ) -> Result<PluginViewLifecycle, OperationError> {
        let mut views = self.views.lock().unwrap();
        let live = views
            .get_mut(view)
            .ok_or_else(|| OperationError::NotFound("view connection".into()))?;
        if context.caller.kind != host::CallerKind::Plugin
            || context.caller.id != view.as_str()
            || context.connection_id != live.connection.connection.as_str()
            || context.principal() != live.context.principal()
        {
            return Err(invalid(
                "close cooperation belongs to the original view channel",
            ));
        }
        let renderer = match request {
            PluginViewRequest::RegisterCloseHandler { renderer }
            | PluginViewRequest::ObserveLifecycle { renderer }
            | PluginViewRequest::PrepareClose { renderer, .. }
            | PluginViewRequest::RefuseClose { renderer, .. } => renderer,
            _ => return Err(invalid("unsupported close cooperation request")),
        };
        if matches!(request, PluginViewRequest::RegisterCloseHandler { .. }) {
            if live.closing.is_some() {
                return Err(OperationError::ContentChanged(
                    "view closure is preparing".into(),
                ));
            }
            if !live.renderers.contains(renderer) && live.renderers.len() >= 32 {
                return Err(invalid("view renderer quota reached"));
            }
            live.renderers.insert(renderer.clone());
        } else if !live.renderers.contains(renderer) {
            return Err(invalid("this document has not registered a close handler"));
        }
        if let PluginViewRequest::PrepareClose { operation, .. }
        | PluginViewRequest::RefuseClose { operation, .. } = request
        {
            let close = live.closing.as_mut().ok_or_else(|| {
                OperationError::ContentChanged("no view close is preparing".into())
            })?;
            if &close.operation != operation {
                return Err(OperationError::ContentChanged(
                    "the original close Operation changed".into(),
                ));
            }
            match request {
                PluginViewRequest::PrepareClose { .. } => {
                    if close.refused.is_some() {
                        return Err(OperationError::ContentChanged(
                            "view close preparation was refused".into(),
                        ));
                    }
                    close.prepared.insert(renderer.clone());
                }
                PluginViewRequest::RefuseClose { reason, .. } => {
                    if reason.trim().is_empty() || reason.len() > 4096 {
                        return Err(invalid("close refusal must contain 1–4096 UTF-8 bytes"));
                    }
                    close.refused = Some(reason.clone());
                }
                _ => unreachable!(),
            }
            self.view_sequences
                .send_modify(|version| *version = version.wrapping_add(1));
        }
        let close = match &live.closing {
            None => PluginViewCloseState::Open,
            Some(close) => {
                if let Some(reason) = &close.refused {
                    PluginViewCloseState::Refused {
                        operation: close.operation.clone(),
                        reason: reason.clone(),
                    }
                } else if close.prepared.contains(renderer) {
                    PluginViewCloseState::Prepared {
                        operation: close.operation.clone(),
                    }
                } else {
                    PluginViewCloseState::Requested {
                        operation: close.operation.clone(),
                    }
                }
            }
        };
        Ok(PluginViewLifecycle {
            view: view.clone(),
            close,
        })
    }

    pub fn check_view_close_fence(
        &self,
        view: &ViewInstanceId,
        request: &PluginViewRequest,
    ) -> Result<(), OperationError> {
        let views = self.views.lock().unwrap();
        let live = views
            .get(view)
            .ok_or_else(|| OperationError::NotFound("view connection".into()))?;
        if let Some(close) = &live.closing {
            let browser_action = matches!(
                request,
                PluginViewRequest::Cancel { .. }
                    | PluginViewRequest::BeginTextCopy
                    | PluginViewRequest::OpenExternalUrl { .. }
                    | PluginViewRequest::DownloadResource { .. }
                    | PluginViewRequest::DownloadArchive { .. }
                    | PluginViewRequest::FinishTextCopy { .. }
            );
            let owner_action = matches!(
                request,
                PluginViewRequest::Invoke { .. } | PluginViewRequest::Control { .. }
            );
            if browser_action || (owner_action && close.sealed(live.renderers.len())) {
                return Err(OperationError::ContentChanged(
                    "view closure is preparing; new actions are fenced".into(),
                ));
            }
        }
        // Owners may use their declared ports while preparing. Core neither
        // identifies their save calls nor exempts them from authority checks.

        Ok(())
    }

    pub(crate) async fn close_view_cooperatively(
        &self,
        context: &host::CallContext,
        operation: OperationId,
        args: ClosePluginView,
    ) -> Result<PluginViewRecord, OperationError> {
        let mut changes = self.view_sequences.subscribe();
        {
            let _guard = self.gate.lock().await;
            let record = self.prepare_view_close(context, &args)?;
            if record.closed {
                return Ok(record);
            }
            if let PluginViewCloseMode::Disconnect { connection } = &args.mode {
                return self.close_view_record(context, &args.view, connection.as_ref());
            }
            self.views
                .lock()
                .unwrap()
                .get_mut(&args.view)
                .unwrap()
                .closing = Some(CloseAttempt {
                operation: operation.clone(),
                prepared: BTreeSet::new(),
                refused: None,
            });
        }
        let _preparation = Preparation {
            service: self,
            view: args.view.clone(),
            operation: operation.clone(),
        };
        self.view_sequences
            .send_modify(|version| *version = version.wrapping_add(1));
        let connection = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                {
                    let views = self.views.lock().unwrap();
                    let live = views.get(&args.view).ok_or_else(|| OperationError::NotFound("view connection ended during closure".into()))?;
                    let close = live.closing.as_ref().ok_or_else(|| OperationError::ContentChanged("view close preparation ended".into()))?;
                    if close.operation != operation { return Err(OperationError::ContentChanged("view close identity changed".into())); }
                    if let Some(reason) = &close.refused { return Err(invalid(format!("participant preparation was refused: {reason}"))); }
                    if close.sealed(live.renderers.len()) {
                        return Ok(live.connection.connection.clone());
                    }
                }
                changes.changed().await.map_err(invalid)?;
            }
        }).await.map_err(|_| invalid("participants did not confirm preparation before the close deadline; the view remains open"))??;
        let _guard = self.gate.lock().await;
        // A document can end while this task waits for the native gate. A
        // smaller participant set must never turn that loss into preparation proof.
        {
            let views = self.views.lock().unwrap();
            let close = views
                .get(&args.view)
                .and_then(|live| live.closing.as_ref())
                .ok_or_else(|| invalid("view close preparation ended"))?;
            if let Some(reason) = &close.refused {
                return Err(invalid(format!(
                    "participant preparation was refused: {reason}"
                )));
            }
        }
        self.close_view_record(context, &args.view, Some(&connection))
    }
}
