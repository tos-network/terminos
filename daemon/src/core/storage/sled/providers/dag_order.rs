use async_trait::async_trait;
use log::trace;
use terminos_common::{
    block::TopoHeight, crypto::Hash, serializer::Serializer
};
use crate::core::{
    error::{BlockchainError, DiskContext},
    storage::{DagOrderProvider, SledStorage},
};

#[async_trait]
impl DagOrderProvider for SledStorage {
    async fn set_topo_height_for_block(&mut self, hash: &Hash, topoheight: TopoHeight) -> Result<(), BlockchainError> {
        trace!("set topo height for {} at {}", hash, topoheight);
        Self::insert_into_disk(self.snapshot.as_mut(), &self.topo_by_hash, hash.as_bytes(), &topoheight.to_be_bytes())?;
        Self::insert_into_disk(self.snapshot.as_mut(), &self.hash_at_topo, &topoheight.to_be_bytes(), hash.as_bytes())?;

        // save in cache
        if let Some(cache) = &self.topo_by_hash_cache {
            let mut topo = cache.lock().await;
            topo.put(hash.clone(), topoheight);
        }

        if let Some(cache) = &self.hash_at_topo_cache {
            let mut hash_at_topo = cache.lock().await;
            hash_at_topo.put(topoheight, hash.clone());
        }

        Ok(())
    }

    async fn is_block_topological_ordered(&self, hash: &Hash) -> Result<bool, BlockchainError> {
        trace!("is block topological ordered: {}", hash);
        let topoheight = match self.get_topo_height_for_hash(&hash).await {
            Ok(topoheight) => topoheight,
            Err(e) => {
                trace!("Error while checking if block {} is ordered: {}", hash, e);
                return Ok(false)
            }
        };

        let hash_at_topo = match self.get_hash_at_topo_height(topoheight).await {
            Ok(hash_at_topo) => hash_at_topo,
            Err(e) => {
                trace!("Error while checking if a block hash is ordered at topo {}: {}", topoheight, e);
                return Ok(false)
            }
        };
        Ok(hash_at_topo == *hash)
    }

    async fn get_topo_height_for_hash(&self, hash: &Hash) -> Result<TopoHeight, BlockchainError> {
        trace!("get topoheight for hash: {}", hash);
        self.get_cacheable_data(&self.topo_by_hash, &self.topo_by_hash_cache, &hash, DiskContext::GetTopoHeightForHash).await
    }

    async fn get_hash_at_topo_height(&self, topoheight: TopoHeight) -> Result<Hash, BlockchainError> {
        trace!("get hash at topoheight: {}", topoheight);
        self.get_cacheable_data(&self.hash_at_topo, &self.hash_at_topo_cache, &topoheight, DiskContext::GetBlockHashAtTopoHeight(topoheight)).await
    }

    async fn has_hash_at_topoheight(&self, topoheight: TopoHeight) -> Result<bool, BlockchainError> {
        trace!("has hash at topoheight {}", topoheight);
        self.contains_data_cached(&self.hash_at_topo, &self.hash_at_topo_cache, &topoheight).await
    }

    async fn get_orphaned_blocks<'a>(&'a self) -> Result<impl Iterator<Item = Result<Hash, BlockchainError>> + 'a, BlockchainError> {
        trace!("get orphaned blocks");

        let iter = Self::iter_keys(self.snapshot.as_ref(), &self.blocks);
        Ok(iter.map(|res| {
            let key = res?;
            let hash = Hash::from_bytes(&key)?;

            if self.contains_data(&self.topo_by_hash, &hash)? {
                return Ok(None);
            }

            Ok(Some(hash))
        }).filter_map(Result::transpose))
    }
}