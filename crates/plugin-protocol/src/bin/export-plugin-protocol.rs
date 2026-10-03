use rho_plugin_protocol::*;
use std::{fs, path::PathBuf};
use ts_rs::{Config, TS};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("expected export directory")?,
    );
    let types = Config::new()
        .with_out_dir(root.join("types"))
        .with_large_int("number");
    OperationId::export_all(&types)?;
    ProcessReport::export_all(&types)?;
    PluginArchive::export_all(&types)?;
    StagePluginArchive::export_all(&types)?;
    PluginArchiveDiscarded::export_all(&types)?;
    PluginArchiveProgress::export_all(&types)?;
    PluginArchiveArguments::export_all(&types)?;
    ReadPluginArchive::export_all(&types)?;
    PluginArchiveChunk::export_all(&types)?;
    ExportPluginArchive::export_all(&types)?;
    PluginArchiveReceipt::export_all(&types)?;
    PluginArchiveInspection::export_all(&types)?;
    PluginArchiveOperationArguments::export_all(&types)?;
    PluginRevisionPage::export_all(&types)?;
    RevisionDifference::export_all(&types)?;
    PluginInstance::export_all(&types)?;
    WorkspacePaths::export_all(&types)?;
    ProjectReadCoverage::export_all(&types)?;
    ProjectReadCoverageArguments::export_all(&types)?;
    PluginInstancePage::export_all(&types)?;
    PluginRequest::export_all(&types)?;
    PluginDelegatedOperationArguments::export_all(&types)?;
    PluginDelegatedOperation::export_all(&types)?;
    HostCapabilityArguments::export_all(&types)?;
    HostCapabilityContract::export_all(&types)?;
    PluginPreflightRequest::export_all(&types)?;
    PluginPreflightResult::export_all(&types)?;
    PluginCatalogArguments::export_all(&types)?;
    PluginRevisionArguments::export_all(&types)?;
    PluginCatalogPage::export_all(&types)?;
    PluginInspection::export_all(&types)?;
    PluginInstancesArguments::export_all(&types)?;
    PluginInstanceArguments::export_all(&types)?;
    PluginInstanceObservations::export_all(&types)?;
    PluginResolveArguments::export_all(&types)?;
    ActivatePlugin::export_all(&types)?;
    ResumePlugin::export_all(&types)?;
    ComparePluginRevisions::export_all(&types)?;
    ResourceInspect::export_all(&types)?;
    ResourceList::export_all(&types)?;
    ResourcePage::export_all(&types)?;
    ResourceChunk::export_all(&types)?;
    ResourceTransferRequest::export_all(&types)?;
    ResourceTransferResponse::export_all(&types)?;
    OpenPluginView::export_all(&types)?;
    UpdatePluginView::export_all(&types)?;
    ReconnectPluginView::export_all(&types)?;
    PluginViewArguments::export_all(&types)?;
    ClosePluginView::export_all(&types)?;
    PluginViewLifecycle::export_all(&types)?;
    ReleasePluginViewRenderer::export_all(&types)?;
    PluginViewRendererRelease::export_all(&types)?;
    PluginViewConnection::export_all(&types)?;
    PluginViewCaller::export_all(&types)?;
    PluginViewPresence::export_all(&types)?;
    PluginViewMessage::export_all(&types)?;
    ContextSearch::export_all(&types)?;
    ContextPage::export_all(&types)?;
    PreviewContext::export_all(&types)?;
    ContextPreview::export_all(&types)?;
    DocumentDraft::export_all(&types)?;
    StageDraftChunk::export_all(&types)?;
    SaveDocumentDraft::export_all(&types)?;
    DocumentDraftArguments::export_all(&types)?;
    ListDocumentDrafts::export_all(&types)?;
    DocumentDraftPage::export_all(&types)?;
    ReadDocumentDraft::export_all(&types)?;
    DocumentDraftChunk::export_all(&types)?;
    DiscardDocumentDraft::export_all(&types)?;
    RpcFrame::export_all(&types)?;
    ListPluginSource::export_all(&types)?;
    PluginSourcePage::export_all(&types)?;
    ReadPluginSource::export_all(&types)?;
    PluginSourceChunk::export_all(&types)?;
    fs::create_dir_all(root.join("schema"))?;
    for (name, schema) in [
        (
            "list-plugin-source",
            schemars::schema_for!(ListPluginSource),
        ),
        (
            "plugin-source-page",
            schemars::schema_for!(PluginSourcePage),
        ),
        (
            "read-plugin-source",
            schemars::schema_for!(ReadPluginSource),
        ),
        (
            "plugin-source-chunk",
            schemars::schema_for!(PluginSourceChunk),
        ),
        (
            "project-read-coverage",
            schemars::schema_for!(ProjectReadCoverage),
        ),
        (
            "project-read-coverage-arguments",
            schemars::schema_for!(ProjectReadCoverageArguments),
        ),
        ("manifest", schemars::schema_for!(PluginManifest)),
        ("archive", schemars::schema_for!(PluginArchive)),
        ("stage-archive", schemars::schema_for!(StagePluginArchive)),
        ("read-archive", schemars::schema_for!(ReadPluginArchive)),
        ("export-archive", schemars::schema_for!(ExportPluginArchive)),
        (
            "archive-reference",
            schemars::schema_for!(PluginArchiveReference),
        ),
        (
            "archive-arguments",
            schemars::schema_for!(PluginArchiveArguments),
        ),
        (
            "archive-progress",
            schemars::schema_for!(PluginArchiveProgress),
        ),
        (
            "archive-discarded",
            schemars::schema_for!(PluginArchiveDiscarded),
        ),
        ("archive-chunk", schemars::schema_for!(PluginArchiveChunk)),
        (
            "archive-receipt",
            schemars::schema_for!(PluginArchiveReceipt),
        ),
        (
            "archive-inspection",
            schemars::schema_for!(PluginArchiveInspection),
        ),
        (
            "archive-operation-arguments",
            schemars::schema_for!(PluginArchiveOperationArguments),
        ),
        ("rpc", schemars::schema_for!(RpcFrame)),
        (
            "delegated-operation-arguments",
            schemars::schema_for!(PluginDelegatedOperationArguments),
        ),
        (
            "delegated-operation",
            schemars::schema_for!(PluginDelegatedOperation),
        ),
        (
            "host-capability-arguments",
            schemars::schema_for!(HostCapabilityArguments),
        ),
        (
            "host-capability-contract",
            schemars::schema_for!(HostCapabilityContract),
        ),
        ("workspace-paths", schemars::schema_for!(WorkspacePaths)),
        ("view-message", schemars::schema_for!(PluginViewMessage)),
        ("view-caller", schemars::schema_for!(PluginViewCaller)),
        ("view-presence", schemars::schema_for!(PluginViewPresence)),
        ("view-close", schemars::schema_for!(ClosePluginView)),
        ("view-reconnect", schemars::schema_for!(ReconnectPluginView)),
        ("plugin-resume", schemars::schema_for!(ResumePlugin)),
        (
            "release-view-renderer",
            schemars::schema_for!(ReleasePluginViewRenderer),
        ),
        (
            "view-renderer-release",
            schemars::schema_for!(PluginViewRendererRelease),
        ),
        ("context-search", schemars::schema_for!(ContextSearch)),
        ("context-page", schemars::schema_for!(ContextPage)),
        ("preview-context", schemars::schema_for!(PreviewContext)),
        ("context-preview", schemars::schema_for!(ContextPreview)),
        ("document-draft", schemars::schema_for!(DocumentDraft)),
        (
            "list-document-drafts",
            schemars::schema_for!(ListDocumentDrafts),
        ),
        (
            "document-draft-page",
            schemars::schema_for!(DocumentDraftPage),
        ),
        (
            "save-document-draft",
            schemars::schema_for!(SaveDocumentDraft),
        ),
        (
            "resource-transfer-request",
            schemars::schema_for!(ResourceTransferRequest),
        ),
        (
            "resource-transfer-response",
            schemars::schema_for!(ResourceTransferResponse),
        ),
    ] {
        fs::write(
            root.join("schema").join(format!("{name}.json")),
            serde_json::to_string_pretty(&schema)?,
        )?;
    }
    Ok(())
}
