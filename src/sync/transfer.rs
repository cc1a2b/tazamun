//! Blob transfer: publishing local files, pulling remote ones, and GC.
//!
//! Invariant: bytes only ever reach the synced folder through a fully
//! verified staging file — every chunk is BLAKE3-verified by iroh-blobs on
//! fetch and length-checked on assembly, the staged file is fsynced, and the
//! final step is an atomic rename. A failed pull leaves the folder untouched.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr};
use iroh_blobs::Hash;
use iroh_blobs::api::TempTag;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::{GcConfig, ProtectOutcome};
use tokio::io::AsyncWriteExt;
use tracing::{debug, instrument};

use crate::consts::{FETCH_CONCURRENCY, INLINE_MANIFEST_MAX, MAX_MANIFEST_BYTES};
use crate::proto::{ChunkRef, FileRecord, ManifestRef};
use crate::state::RelPath;
use crate::sync::{chunker, manifest};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("chunking: {0}")]
    Chunk(#[from] chunker::ChunkError),
    #[error("blob store: {0}")]
    Store(String),
    #[error("fetch from peer: {0}")]
    Fetch(String),
    #[error("manifest invalid: {0}")]
    ManifestInvalid(String),
    #[error("chunk length mismatch for {0}")]
    ChunkMismatch(String),
    #[error("task join: {0}")]
    Join(String),
}

fn store_err(e: impl std::fmt::Display) -> TransferError {
    TransferError::Store(e.to_string())
}

/// Result of publishing a local file into the blob store.
pub struct Published {
    pub manifest: ManifestRef,
    pub size: u64,
    /// Temp tags keeping the new blobs alive until the daemon commits the
    /// record into persistent state.
    pub tags: Vec<TempTag>,
}

/// A fully verified staged file, ready for atomic rename into the folder.
pub struct Staged {
    pub temp: tempfile::TempPath,
    pub size: u64,
    /// Temp tags protecting the involved blobs until commit.
    pub tags: Vec<TempTag>,
}

/// Owns the persistent iroh-blobs store for one session folder.
///
/// Garbage collection runs inside the store on `gc_interval`; the store asks
/// back through a protect callback that reads the snapshot maintained via
/// [`Transfer::set_protected`]. In-flight operations additionally hold
/// [`TempTag`]s, so a sweep can never collect bytes that are being staged.
#[derive(Debug, Clone)]
pub struct Transfer {
    store: FsStore,
    root: PathBuf,
    tmp_dir: PathBuf,
    protected: Arc<Mutex<HashSet<Hash>>>,
}

impl Transfer {
    pub async fn open(root: PathBuf, gc_interval: Duration) -> Result<Self, TransferError> {
        let blobs = crate::state::blobs_dir(&root);
        let tmp_dir = crate::state::tmp_dir(&root);
        std::fs::create_dir_all(&blobs)?;
        std::fs::create_dir_all(&tmp_dir)?;
        let protected: Arc<Mutex<HashSet<Hash>>> = Arc::new(Mutex::new(HashSet::new()));
        let cb_protected = protected.clone();
        let mut options = iroh_blobs::store::fs::options::Options::new(&blobs);
        options.gc = Some(GcConfig {
            interval: gc_interval,
            add_protected: Some(Arc::new(move |live: &mut HashSet<Hash>| {
                let snapshot = cb_protected.clone();
                Box::pin(async move {
                    match snapshot.lock() {
                        Ok(guard) => {
                            live.extend(guard.iter().copied());
                            ProtectOutcome::Continue
                        }
                        // A poisoned snapshot means a panic elsewhere; skip
                        // the run rather than sweep blindly.
                        Err(_) => ProtectOutcome::Abort,
                    }
                })
            })),
        });
        let store = FsStore::load_with_opts(blobs.join("blobs.db"), options)
            .await
            .map_err(store_err)?;
        Ok(Self {
            store,
            root,
            tmp_dir,
            protected,
        })
    }

    /// Replaces the GC-protected snapshot.
    pub fn set_protected(&self, live: HashSet<Hash>) -> usize {
        match self.protected.lock() {
            Ok(mut guard) => {
                *guard = live;
                guard.len()
            }
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = live;
                guard.len()
            }
        }
    }

    pub fn store(&self) -> &FsStore {
        &self.store
    }

    pub async fn shutdown(&self) {
        let _ = self.store.shutdown().await;
    }

    /// Streams `rel` through the chunker into the store and returns its
    /// manifest. Deduplication is by chunk hash: existing blobs are no-ops.
    #[instrument(skip(self), fields(path = %rel))]
    pub async fn publish_local(&self, rel: &RelPath) -> Result<Published, TransferError> {
        let abs = rel.to_fs_path(&self.root);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        let chunk_task = tokio::task::spawn_blocking(move || {
            chunker::chunk_file(&abs, |_, data| {
                tx.blocking_send(data)
                    .map_err(|_| chunker::ChunkError::Io(std::io::Error::other("sink closed")))
            })
        });

        let mut tags = Vec::new();
        while let Some(data) = rx.recv().await {
            let tag = self
                .store
                .blobs()
                .add_bytes(data)
                .temp_tag()
                .await
                .map_err(store_err)?;
            tags.push(tag);
        }
        let (refs, size) = chunk_task
            .await
            .map_err(|e| TransferError::Join(e.to_string()))??;

        // The store hashes with BLAKE3 exactly like the chunker; a mismatch
        // would mean corruption between the two passes.
        for (tag, r) in tags.iter().zip(&refs) {
            if tag.hash_and_format().hash != Hash::from_bytes(r.hash) {
                return Err(TransferError::ChunkMismatch(rel.to_string()));
            }
        }

        let (manifest, manifest_tag) = self.build_manifest(refs).await?;
        if let Some(t) = manifest_tag {
            tags.push(t);
        }
        Ok(Published {
            manifest,
            size,
            tags,
        })
    }

    /// Inline small manifests; spill large ones into a postcard blob.
    async fn build_manifest(
        &self,
        refs: Vec<ChunkRef>,
    ) -> Result<(ManifestRef, Option<TempTag>), TransferError> {
        if refs.len() <= INLINE_MANIFEST_MAX {
            return Ok((ManifestRef::Inline(refs), None));
        }
        let bytes = postcard::to_stdvec(&refs)
            .map_err(|e| TransferError::ManifestInvalid(e.to_string()))?;
        let tag = self
            .store
            .blobs()
            .add_bytes(bytes)
            .temp_tag()
            .await
            .map_err(store_err)?;
        let hash = *tag.hash_and_format().hash.as_bytes();
        Ok((ManifestRef::Blob { hash }, Some(tag)))
    }

    /// Resolves a manifest into its chunk list, fetching the manifest blob
    /// from `conn` when provided and not locally present.
    async fn resolve_manifest(
        &self,
        manifest: &ManifestRef,
        expected_size: u64,
        conn: Option<&iroh::endpoint::Connection>,
        tags: &mut Vec<TempTag>,
    ) -> Result<Vec<ChunkRef>, TransferError> {
        let refs = match manifest {
            ManifestRef::Inline(refs) => refs.clone(),
            ManifestRef::Blob { hash } => {
                let h = Hash::from_bytes(*hash);
                let have = self.store.blobs().has(h).await.map_err(store_err)?;
                if !have {
                    let conn = conn.ok_or_else(|| {
                        TransferError::ManifestInvalid("manifest blob not in local store".into())
                    })?;
                    self.store
                        .remote()
                        .fetch(conn.clone(), h)
                        .await
                        .map_err(|e| TransferError::Fetch(e.to_string()))?;
                }
                tags.push(self.store.tags().temp_tag(h).await.map_err(store_err)?);
                // DoS bound: a manifest blob is untrusted peer input. Its size
                // is known from the verified store entry, so refuse to load one
                // larger than MAX_MANIFEST_BYTES into memory before `get_bytes`.
                if let iroh_blobs::api::proto::BlobStatus::Complete { size } =
                    self.store.blobs().status(h).await.map_err(store_err)?
                    && size > MAX_MANIFEST_BYTES
                {
                    return Err(TransferError::ManifestInvalid(format!(
                        "manifest blob is {size} bytes, exceeds the {MAX_MANIFEST_BYTES}-byte cap"
                    )));
                }
                let bytes = self.store.blobs().get_bytes(h).await.map_err(store_err)?;
                // Decode with the chunk-count cap enforced (pure, shared with
                // the fuzz harness and the manifest-bomb regression tests).
                manifest::decode_blob(&bytes)
                    .map_err(|e| TransferError::ManifestInvalid(e.to_string()))?
            }
        };
        // Count cap + checked (overflow-audited) length fold equal to the
        // record's declared size. Rejects chunk-count/size bombs before any
        // allocation proportional to the claim.
        manifest::check(&refs, expected_size)
            .map_err(|e| TransferError::ManifestInvalid(e.to_string()))?;
        Ok(refs)
    }

    /// Pulls one remote record: fetches only locally missing chunks from
    /// `from` (≤ [`FETCH_CONCURRENCY`] in flight, BLAKE3-verified by
    /// iroh-blobs) and assembles a verified staging file. `meter`, when
    /// given, receives byte progress for the terminal bar and `status` rows.
    #[instrument(skip(self, endpoint, record, meter), fields(peer = %from.id.fmt_short(), path = %rel))]
    pub async fn pull_stage(
        &self,
        endpoint: &Endpoint,
        from: EndpointAddr,
        rel: &RelPath,
        record: &FileRecord,
        meter: Option<Arc<crate::ui::progress::Meter>>,
    ) -> Result<Staged, TransferError> {
        // Connect lazily: an inline manifest whose chunks are all local (or an
        // empty file) needs no network at all — the pull completes from the
        // store alone.
        let mut conn: Option<iroh::endpoint::Connection> = None;
        let mut tags = Vec::new();
        let refs = if let ManifestRef::Inline(refs) = &record.manifest {
            refs.clone()
        } else {
            let c = endpoint
                .connect(from.clone(), iroh_blobs::ALPN)
                .await
                .map_err(|e| TransferError::Fetch(e.to_string()))?;
            let refs = self
                .resolve_manifest(&record.manifest, record.size, Some(&c), &mut tags)
                .await?;
            conn = Some(c);
            refs
        };
        if let Some(m) = &meter {
            m.set_chunks(refs.len());
        }

        // Unique hashes still missing locally; count file bytes per unique
        // hash so progress reflects coverage of the whole file (duplicate
        // chunks are fetched once but advance the bar by all occurrences).
        let mut bytes_by_hash: std::collections::HashMap<[u8; 32], u64> =
            std::collections::HashMap::new();
        for r in &refs {
            *bytes_by_hash.entry(r.hash).or_insert(0) += u64::from(r.len);
        }
        let mut missing: Vec<Hash> = Vec::new();
        let mut seen: HashSet<[u8; 32]> = HashSet::new();
        let mut already_local: u64 = 0;
        for r in &refs {
            if seen.insert(r.hash) {
                let h = Hash::from_bytes(r.hash);
                if self.store.blobs().has(h).await.map_err(store_err)? {
                    already_local += bytes_by_hash.get(&r.hash).copied().unwrap_or(0);
                } else {
                    missing.push(h);
                }
            }
        }
        if already_local > 0
            && let Some(m) = &meter
        {
            m.inc(already_local);
        }
        let missing_count = missing.len();

        // Only dial when something actually has to cross the network.
        if missing_count > 0 && conn.is_none() {
            conn = Some(
                endpoint
                    .connect(from, iroh_blobs::ALPN)
                    .await
                    .map_err(|e| TransferError::Fetch(e.to_string()))?,
            );
        }
        let mut queue = missing.into_iter();
        let mut inflight = tokio::task::JoinSet::new();
        loop {
            while inflight.len() < FETCH_CONCURRENCY {
                let Some(h) = queue.next() else { break };
                let Some(conn) = conn.clone() else { break };
                let store = self.store.clone();
                inflight.spawn(async move {
                    store
                        .remote()
                        .fetch(conn, h)
                        .await
                        .map(|_| h)
                        .map_err(|e| TransferError::Fetch(e.to_string()))
                });
            }
            match inflight.join_next().await {
                None => break,
                Some(res) => {
                    let h = res.map_err(|e| TransferError::Join(e.to_string()))??;
                    tags.push(self.store.tags().temp_tag(h).await.map_err(store_err)?);
                    if let Some(m) = &meter {
                        m.inc(bytes_by_hash.get(h.as_bytes()).copied().unwrap_or(0));
                    }
                }
            }
        }
        debug!(fetched = missing_count, chunks = refs.len(), "pull fetched");
        if let Some(m) = &meter {
            m.finish();
        }

        let staged = self.assemble(&refs).await?;
        Ok(Staged {
            temp: staged,
            size: record.size,
            tags,
        })
    }

    /// Materializes a manifest that is fully present in the local store
    /// (used by `restore` and by violation recovery).
    pub async fn materialize(
        &self,
        manifest: &ManifestRef,
        expected_size: u64,
    ) -> Result<Staged, TransferError> {
        let mut tags = Vec::new();
        let refs = self
            .resolve_manifest(manifest, expected_size, None, &mut tags)
            .await?;
        let temp = self.assemble(&refs).await?;
        Ok(Staged {
            temp,
            size: expected_size,
            tags,
        })
    }

    /// Writes the chunks in order into a fresh staging file and fsyncs it.
    async fn assemble(&self, refs: &[ChunkRef]) -> Result<tempfile::TempPath, TransferError> {
        std::fs::create_dir_all(&self.tmp_dir)?;
        let named = tempfile::Builder::new()
            .prefix("stage-")
            .tempfile_in(&self.tmp_dir)?;
        let (file, temp_path) = named.into_parts();
        let mut out = tokio::fs::File::from_std(file);
        for r in refs {
            let bytes = self
                .store
                .blobs()
                .get_bytes(Hash::from_bytes(r.hash))
                .await
                .map_err(store_err)?;
            if bytes.len() as u32 != r.len {
                return Err(TransferError::ChunkMismatch(format!(
                    "{} has {} bytes, manifest says {}",
                    Hash::from_bytes(r.hash),
                    bytes.len(),
                    r.len
                )));
            }
            out.write_all(&bytes).await?;
        }
        out.sync_all().await?;
        drop(out);
        Ok(temp_path)
    }

    /// Verifies whether the on-disk bytes of `rel` still match `record`'s
    /// manifest (size fast-path, then full chunk hashing off-thread).
    pub async fn disk_matches(
        &self,
        rel: &RelPath,
        record: &FileRecord,
    ) -> Result<bool, TransferError> {
        let abs = rel.to_fs_path(&self.root);
        let meta = match std::fs::metadata(&abs) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(record.deleted);
            }
            Err(e) => return Err(e.into()),
        };
        if record.deleted || meta.len() != record.size {
            return Ok(false);
        }
        let mut tags = Vec::new();
        let want = self
            .resolve_manifest(&record.manifest, record.size, None, &mut tags)
            .await?;
        let got = tokio::task::spawn_blocking(move || -> Result<Vec<ChunkRef>, TransferError> {
            let (refs, _) = chunker::chunk_file(&abs, |_, _| Ok(()))?;
            Ok(refs)
        })
        .await
        .map_err(|e| TransferError::Join(e.to_string()))??;
        Ok(want == got)
    }

    /// Computes the blob set reachable from committed state: every chunk hash
    /// and manifest-blob hash referenced by the current index and all history
    /// entries (expanding manifest blobs into their chunk lists).
    #[instrument(skip_all)]
    pub async fn compute_live(
        &self,
        files: &BTreeMap<RelPath, FileRecord>,
        history: &BTreeMap<RelPath, Vec<crate::state::VersionEntry>>,
    ) -> Result<HashSet<Hash>, TransferError> {
        let mut live: HashSet<Hash> = HashSet::new();
        let manifests = files
            .values()
            .map(|r| &r.manifest)
            .chain(history.values().flatten().map(|e| &e.manifest));
        for manifest in manifests {
            match manifest {
                ManifestRef::Inline(refs) => {
                    live.extend(refs.iter().map(|r| Hash::from_bytes(r.hash)));
                }
                ManifestRef::Blob { hash } => {
                    let h = Hash::from_bytes(*hash);
                    live.insert(h);
                    // Protect the chunks listed inside the manifest blob too.
                    if self.store.blobs().has(h).await.map_err(store_err)? {
                        let bytes = self.store.blobs().get_bytes(h).await.map_err(store_err)?;
                        if let Ok(refs) = postcard::from_bytes::<Vec<ChunkRef>>(&bytes) {
                            live.extend(refs.iter().map(|r| Hash::from_bytes(r.hash)));
                        }
                    }
                }
            }
        }
        Ok(live)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::CDC_MAX;
    use crate::sync::index::sanitize_rel_path;

    fn pseudo_random(n: usize) -> Vec<u8> {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.truncate(n);
        out
    }

    #[tokio::test]
    async fn manifest_inline_blob_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let t = Transfer::open(dir.path().to_path_buf(), Duration::from_secs(3600))
            .await
            .unwrap();

        // At most INLINE_MANIFEST_MAX chunks → inline.
        let small: Vec<ChunkRef> = (0..INLINE_MANIFEST_MAX as u32)
            .map(|i| ChunkRef {
                hash: blake3::hash(&i.to_le_bytes()).into(),
                len: 1,
            })
            .collect();
        let (m, tag) = t.build_manifest(small.clone()).await.unwrap();
        assert_eq!(m, ManifestRef::Inline(small));
        assert!(tag.is_none());

        // One more chunk → spills into a manifest blob that resolves back.
        let big: Vec<ChunkRef> = (0..=INLINE_MANIFEST_MAX as u32)
            .map(|i| ChunkRef {
                hash: blake3::hash(&i.to_le_bytes()).into(),
                len: 1,
            })
            .collect();
        let (m, tag) = t.build_manifest(big.clone()).await.unwrap();
        let ManifestRef::Blob { hash } = m else {
            panic!("expected blob manifest");
        };
        assert!(tag.is_some());
        let bytes = t
            .store
            .blobs()
            .get_bytes(Hash::from_bytes(hash))
            .await
            .unwrap();
        let back: Vec<ChunkRef> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, big);
        t.shutdown().await;
    }

    #[tokio::test]
    async fn publish_materialize_roundtrip_and_disk_match() {
        let dir = tempfile::tempdir().unwrap();
        let t = Transfer::open(dir.path().to_path_buf(), Duration::from_secs(3600))
            .await
            .unwrap();
        let rel = sanitize_rel_path("data.bin").unwrap();
        let abs = rel.to_fs_path(dir.path());
        let data = pseudo_random(2 * CDC_MAX as usize + 123);
        std::fs::write(&abs, &data).unwrap();

        let published = t.publish_local(&rel).await.unwrap();
        assert_eq!(published.size, data.len() as u64);
        let record = FileRecord {
            size: published.size,
            manifest: published.manifest.clone(),
            vv: Default::default(),
            deleted: false,
            updated_at_ms: 0,
        };
        assert!(t.disk_matches(&rel, &record).await.unwrap());

        let staged = t.materialize(&record.manifest, record.size).await.unwrap();
        let out = std::fs::read(&staged.temp).unwrap();
        assert_eq!(out, data);

        // Tampering flips the match.
        std::fs::write(&abs, b"tampered").unwrap();
        assert!(!t.disk_matches(&rel, &record).await.unwrap());
        t.shutdown().await;
    }

    #[tokio::test]
    async fn empty_file_publishes_zero_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let t = Transfer::open(dir.path().to_path_buf(), Duration::from_secs(3600))
            .await
            .unwrap();
        let rel = sanitize_rel_path("empty").unwrap();
        std::fs::write(rel.to_fs_path(dir.path()), b"").unwrap();
        let p = t.publish_local(&rel).await.unwrap();
        assert_eq!(p.size, 0);
        assert_eq!(p.manifest, ManifestRef::Inline(vec![]));
        let staged = t.materialize(&p.manifest, 0).await.unwrap();
        assert_eq!(std::fs::read(&staged.temp).unwrap().len(), 0);
        t.shutdown().await;
    }
}
