use crate::{PluginError, PluginRepository, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rho_plugin_protocol::*;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ops::Range};

impl PluginRepository {
    pub fn source_page(&self, args: &ListPluginSource) -> Result<PluginSourcePage, PluginError> {
        ensure(
            (1..=100).contains(&args.limit),
            "source page size must be 1–100",
        )?;
        let revision = self.revision(&args.revision)?;
        let mut entries = revision
            .files
            .iter()
            .filter(|(path, _)| args.after.as_ref().is_none_or(|after| *path > after));
        let files: BTreeMap<_, _> = entries
            .by_ref()
            .take(args.limit as usize)
            .map(|(path, file)| (path.clone(), file.clone()))
            .collect();
        let next = entries
            .next()
            .and_then(|_| files.last_key_value().map(|(path, _)| path.clone()));
        Ok(PluginSourcePage {
            revision: revision.id,
            total: revision.files.len() as u64,
            files,
            next,
        })
    }

    pub fn read_source(&self, args: &ReadPluginSource) -> Result<PluginSourceChunk, PluginError> {
        ensure(
            (1..=MAX_SOURCE_CHUNK_BYTES).contains(&args.limit),
            "source read size must be 1–65536 bytes",
        )?;
        let transaction = self.connection.unchecked_transaction()?;
        let revision = self.revision(&args.revision)?;
        let file = source_file(&revision, &args.path)?.clone();
        ensure(
            args.offset <= file.bytes,
            "source offset exceeds file length",
        )?;
        let end = file
            .bytes
            .min(args.offset.saturating_add(u64::from(args.limit)));
        let bytes = self.verified_source_bytes(&file, args.offset..end)?;
        transaction.commit()?;
        Ok(PluginSourceChunk {
            revision: revision.id,
            path: args.path.clone(),
            file: file.clone(),
            offset: args.offset,
            content_base64: STANDARD.encode(bytes),
            next_offset: (end < file.bytes).then_some(end),
        })
    }

    // Stream the full immutable digest in bounded slices. In particular, corruption outside
    // the requested slice cannot be disguised as a successful partial source observation.
    fn verified_source_bytes(
        &self,
        file: &PackageFile,
        keep: Range<u64>,
    ) -> Result<Vec<u8>, PluginError> {
        ensure(
            file.bytes <= MAX_PACKAGE_BYTES && keep.start <= keep.end && keep.end <= file.bytes,
            "source file exceeds package quota",
        )?;
        let length: u64 = self
            .connection
            .query_row(
                "SELECT length(bytes) FROM blobs WHERE digest=?",
                [file.digest.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| PluginError::Missing(file.digest.to_string()))?;
        ensure(length == file.bytes, "stored source length mismatch")?;
        let mut query = self
            .connection
            .prepare("SELECT substr(bytes,?2,?3) FROM blobs WHERE digest=?1")?;
        let mut hash = Sha256::new();
        let mut kept = Vec::with_capacity((keep.end - keep.start) as usize);
        let mut offset = 0;
        while offset < length {
            let size = (length - offset).min(u64::from(MAX_SOURCE_CHUNK_BYTES));
            let bytes: Vec<u8> = query
                .query_row(params![file.digest.as_str(), offset + 1, size], |row| {
                    row.get(0)
                })?;
            ensure(
                bytes.len() as u64 == size,
                "stored source slice is incomplete",
            )?;
            hash.update(&bytes);
            let from = offset.max(keep.start);
            let to = (offset + size).min(keep.end);
            if from < to {
                kept.extend_from_slice(&bytes[(from - offset) as usize..(to - offset) as usize]);
            }
            offset += size;
        }
        ensure(
            format!("sha256:{:x}", hash.finalize()) == file.digest.as_str(),
            "stored source digest mismatch",
        )?;
        Ok(kept)
    }
}

fn source_file<'a>(
    revision: &'a PluginRevision,
    path: &PackagePath,
) -> Result<&'a PackageFile, PluginError> {
    ensure(
        !path.is_artifact(),
        "source reads cannot address build artifacts",
    )?;
    revision
        .files
        .get(path)
        .ok_or_else(|| PluginError::Missing(format!("source {path} in {}", revision.id)))
}
