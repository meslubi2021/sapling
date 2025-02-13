/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Implement traits from other crates.

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use storemodel::ReadFileContents;
use storemodel::TreeFormat;
use storemodel::TreeStore;
use types::HgId;
use types::Key;
use types::RepoPath;

use crate::GitStore;

#[async_trait]
impl ReadFileContents for GitStore {
    async fn read_file_contents(
        &self,
        keys: Vec<Key>,
    ) -> BoxStream<anyhow::Result<(minibytes::Bytes, Key)>> {
        let iter = keys.into_iter().map(|k| {
            let id = k.hgid;
            let data = self.read_obj(id, git2::ObjectType::Blob)?;
            Ok((data.into(), k))
        });
        futures::stream::iter(iter).boxed()
    }

    async fn read_rename_metadata(
        &self,
        _keys: Vec<Key>,
    ) -> BoxStream<anyhow::Result<(Key, Option<Key>)>> {
        futures::stream::empty().boxed()
    }

    fn refresh(&self) -> anyhow::Result<()> {
        // We don't hold state in memory, so no need to refresh.
        Ok(())
    }
}

impl TreeStore for GitStore {
    fn get(&self, _path: &RepoPath, hgid: HgId) -> anyhow::Result<minibytes::Bytes> {
        let data = self.read_obj(hgid, git2::ObjectType::Tree)?;
        Ok(data.into())
    }

    fn insert(&self, _path: &RepoPath, hgid: HgId, data: minibytes::Bytes) -> anyhow::Result<()> {
        let id = self.write_obj(git2::ObjectType::Tree, data.as_ref())?;
        if id != hgid {
            anyhow::bail!("tree id mismatch: {} (written) != {} (expected)", id, hgid);
        }
        Ok(())
    }

    fn format(&self) -> TreeFormat {
        TreeFormat::Git
    }

    fn refresh(&self) -> anyhow::Result<()> {
        // We don't hold state in memory, so no need to refresh.
        Ok(())
    }
}
