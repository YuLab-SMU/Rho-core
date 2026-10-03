//! Public Host calls and accepted-task ownership shared by every transport.
use crate::{NextHost, OperationError, port_contracts};
use rho_contract::{
    CallContext, Invocation, OperationEventRecord, OperationId, OperationRecord, OutboxRecord,
    QueryRequest, QuerySnapshot,
};
use rho_operation::{CancellationRequestOutcome, StoredDomainFact};
use serde_json::json;

impl NextHost {
    pub async fn dispatch(
        &self,
        context: &CallContext,
        request: rho_contract::HostRequest,
    ) -> Result<serde_json::Value, OperationError> {
        use rho_contract::HostRequest;
        let request = match request {
            HostRequest::Control(control) => {
                port_contracts::control_request(&self.runtime.registry, context, control)?
            }
            request => request,
        };
        let result = match request {
            HostRequest::Control(request) => {
                let runtime = self.runtime.clone();
                let context = context.clone();
                return self
                    .tasks
                    .spawn(async move { runtime.registry.control(&context, request).await })
                    .await
                    .map_err(|_| {
                        OperationError::Unavailable(
                            "Control completion was lost; inspect the native request before retrying"
                                .into(),
                        )
                    })?;
            }
            HostRequest::Invoke(invocation) => {
                serde_json::to_value(if invocation.return_after_acceptance == Some(true) {
                    self.invoke_accepted(context, invocation.invocation).await?
                } else {
                    self.invoke(context, invocation.invocation).await?
                })
            }
            HostRequest::GetOperation { operation_id } => {
                serde_json::to_value(self.get_operation(context, &operation_id).await?)
            }
            HostRequest::RequestCancellation {
                operation_id,
                only_if_pending,
            } => serde_json::to_value(
                self.request_cancellation_conditional(
                    context,
                    &operation_id,
                    only_if_pending.unwrap_or(false),
                )
                .await?,
            ),
            HostRequest::ReconcileCommit(args) => {
                serde_json::to_value(self.reconcile_commit(context, &args).await?)
            }
            HostRequest::QuerySnapshot(query) => {
                serde_json::to_value(self.query_snapshot(context, query).await?)
            }
            HostRequest::Subscribe {
                after_sequence,
                limit,
            } => serde_json::to_value(self.outbox(context, after_sequence, limit).await?),
        };
        result.map_err(|error| OperationError::Contract(error.to_string()))
    }

    pub async fn invoke(
        &self,
        context: &CallContext,
        invocation: Invocation,
    ) -> Result<OperationRecord, OperationError> {
        self.refresh_plugin_registrations();
        // The host owns execution. Dropping an edge's response future must not abandon
        // the result commit or release plugin ownership while work is still running.
        let runtime = self.runtime.clone();
        let context = context.clone();
        self.tasks
            .spawn(async move {
                let result = runtime.gateway.invoke(&context, invocation).await;
                drop(runtime);
                result
            })
            .await
            .map_err(|error| {
                OperationError::Storage(format!("operation task ended without a result: {error}"))
            })?
    }

    pub async fn invoke_accepted(
        &self,
        context: &CallContext,
        invocation: Invocation,
    ) -> Result<OperationRecord, OperationError> {
        self.refresh_plugin_registrations();
        let runtime = self.runtime.clone();
        let context = context.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut task = self.tasks.spawn(async move {
            let result = runtime
                .gateway
                .invoke_notifying(&context, invocation, Some(tx))
                .await;
            drop(runtime);
            result
        });
        tokio::select! {
            record = rx => match record {
                Ok(record) => Ok(record),
                Err(_) => task.await.map_err(|error| OperationError::Storage(error.to_string()))?,
            },
            result = &mut task => result.map_err(|error| OperationError::Storage(error.to_string()))?,
        }
    }

    pub async fn query_snapshot(
        &self,
        context: &CallContext,
        request: QueryRequest,
    ) -> Result<QuerySnapshot, OperationError> {
        self.refresh_plugin_registrations();
        let runtime = self.runtime.clone();
        let context = context.clone();
        // Keep provider ownership until the read has finished, even if an edge disconnects.
        self.tasks
            .spawn(async move {
                let result = runtime.queries.query(&context, request).await;
                drop(runtime);
                result
            })
            .await
            .map_err(|error| OperationError::Storage(format!("query task failed: {error}")))?
    }

    pub async fn get_operation(
        &self,
        context: &CallContext,
        operation_id: &OperationId,
    ) -> Result<Option<OperationRecord>, OperationError> {
        let snapshot = self
            .query_snapshot(
                context,
                QueryRequest {
                    capability: rho_contract::CapabilityRef::new("operation.get", 1)?,
                    arguments: json!({"operation_id":operation_id}),
                },
            )
            .await?;
        let result: rho_contract::OperationGetResult =
            serde_json::from_value(snapshot.data.ok_or_else(|| {
                OperationError::Contract("operation.get returned no payload".into())
            })?)
            .map_err(|e| OperationError::Contract(e.to_string()))?;
        Ok(result.record)
    }

    pub async fn reconcile_commit(
        &self,
        context: &CallContext,
        args: &rho_contract::ReconcileOperationCommit,
    ) -> Result<OperationRecord, OperationError> {
        // The Host owns the completion attempt even if its requesting edge
        // disconnects. Quit must wait for the original lease callback as well.
        let runtime = self.runtime.clone();
        let context = context.clone();
        let args = args.clone();
        self.tasks
            .spawn(async move {
                let capability = rho_contract::CapabilityRef::new(port_contracts::RECONCILE, 1)?;
                runtime
                    .registry
                    .validate_control_input(&context, &capability, &json!(args))?;
                let record = runtime.gateway.reconcile_commit(&context, &args).await?;
                runtime
                    .registry
                    .validate_control_output(&capability, &json!(record))?;
                if let Some(plugins) = &runtime.plugins
                    && let Err(error) = plugins.complete_record(&context, &record).await
                {
                    eprintln!("committed operation retains plugin protections; use plugins.reconcile_references: {error}");
                }
                Ok(record)
            })
            .await
            .map_err(|error| {
                OperationError::Storage(format!("commit reconciliation task ended: {error}"))
            })?
    }

    pub async fn request_cancellation(
        &self,
        context: &CallContext,
        operation_id: &OperationId,
    ) -> Result<CancellationRequestOutcome, OperationError> {
        self.request_cancellation_conditional(context, operation_id, false)
            .await
    }

    async fn request_cancellation_conditional(
        &self,
        context: &CallContext,
        operation_id: &OperationId,
        only_if_pending: bool,
    ) -> Result<CancellationRequestOutcome, OperationError> {
        let runtime = self.runtime.clone();
        let context = context.clone();
        let operation_id = operation_id.clone();
        // Native pending-cancellation preparation can outlive a view/edge. Keep
        // its original journal decision and signal owned by this Host task.
        self.tasks
            .spawn(async move {
                let result = runtime
                    .registry
                    .control(
                        &context,
                        rho_contract::ControlRequest {
                            capability: rho_contract::CapabilityRef::new(port_contracts::CANCEL, 1)?,
                            arguments: json!({"operation_id":operation_id,"only_if_pending":only_if_pending}),
                        },
                    )
                    .await?;
                serde_json::from_value(result).map_err(|error| OperationError::Contract(error.to_string()))
            })
            .await
            .map_err(|_| {
                OperationError::Unavailable(
                    "Original cancellation acknowledgement was lost; inspect the same operation before retrying"
                        .into(),
                )
            })?
    }

    pub async fn events(
        &self,
        context: &CallContext,
        operation_id: &OperationId,
    ) -> Result<Vec<OperationEventRecord>, OperationError> {
        self.runtime.gateway.events(context, operation_id).await
    }

    pub async fn outbox(
        &self,
        context: &CallContext,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<OutboxRecord>, OperationError> {
        let snapshot = self
            .query_snapshot(
                context,
                QueryRequest {
                    capability: rho_contract::CapabilityRef::new(port_contracts::EVENTS, 1)?,
                    arguments: json!({"after_sequence":after_sequence,"limit":limit}),
                },
            )
            .await?;
        let page: rho_contract::OperationEventsPage =
            serde_json::from_value(snapshot.data.ok_or_else(|| {
                OperationError::Contract("operation.events returned no payload".into())
            })?)
            .map_err(|e| OperationError::Contract(e.to_string()))?;
        Ok(page.events)
    }

    pub async fn facts_for_operation(
        &self,
        context: &CallContext,
        operation_id: &OperationId,
    ) -> Result<Vec<StoredDomainFact>, OperationError> {
        self.runtime
            .gateway
            .facts_for_operation(context, operation_id)
            .await
    }
}
