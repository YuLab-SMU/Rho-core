//! Bounded read-only observations of immutable packaged source.
use crate::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

pub const MAX_SOURCE_CHUNK_BYTES: u32 = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ListPluginSource {
    pub revision: RevisionId,
    pub after: Option<PackagePath>,
    #[schemars(range(min = 1, max = 100))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct PluginSourcePage {
    pub revision: RevisionId,
    pub files: BTreeMap<PackagePath, PackageFile>,
    pub total: u64,
    pub next: Option<PackagePath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ReadPluginSource {
    pub revision: RevisionId,
    pub path: PackagePath,
    pub offset: u64,
    #[schemars(range(min = 1, max = 65536))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct PluginSourceChunk {
    pub revision: RevisionId,
    pub path: PackagePath,
    pub file: PackageFile,
    pub offset: u64,
    /// Binary-safe bytes. The owner verifies the full stored file digest before returning.
    pub content_base64: String,
    pub next_offset: Option<u64>,
}
