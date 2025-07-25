use async_trait::async_trait;
use log::trace;
use terminos_common::{
    account::{
        AccountSummary,
        Balance,
        BalanceType,
        VersionedBalance
    },
    block::TopoHeight,
    crypto::{
        Hash,
        PublicKey
    },
    serializer::Serializer
};
use crate::core::{
    error::{BlockchainError, DiskContext},
    storage::{AssetProvider, BalanceProvider, NetworkProvider, SledStorage}
};

impl SledStorage {
    // Generate a key including the key and its asset
    // It is used to store/retrieve the highest topoheight version available
    pub fn get_balance_key_for(&self, key: &PublicKey, asset: &Hash) -> [u8; 64] {
        trace!("get balance {} key for {}", asset, key.as_address(self.is_mainnet()));
        let mut bytes = [0; 64];
        bytes[0..32].copy_from_slice(key.as_bytes());
        bytes[32..64].copy_from_slice(asset.as_bytes());
        bytes
    }

    // Versioned key is a 72 bytes key with topoheight, key, assets bytes
    pub fn get_versioned_balance_key(&self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> [u8; 72] {
        trace!("get versioned balance {} key at {} for {}", asset, topoheight, key.as_address(self.is_mainnet()));
        let mut bytes = [0; 72];
        bytes[0..8].copy_from_slice(&topoheight.to_be_bytes());
        bytes[8..40].copy_from_slice(key.as_bytes());
        bytes[40..72].copy_from_slice(asset.as_bytes());

        bytes
    }

    async fn has_balance_internal(&self, key: &[u8; 64]) -> Result<bool, BlockchainError> {
        trace!("has balance internal");
        self.contains_data(&self.balances, key)
    }

}

#[async_trait]
impl BalanceProvider for SledStorage {
    // Check if a balance exists for asset and key
    async fn has_balance_for(&self, key: &PublicKey, asset: &Hash) -> Result<bool, BlockchainError> {
        trace!("has balance {} for {}", asset, key.as_address(self.is_mainnet()));
        if !self.has_asset(asset).await? {
            return Err(BlockchainError::AssetNotFound(asset.clone()))
        }

        self.has_balance_internal(&self.get_balance_key_for(key, asset)).await
    }

    // returns the highest topoheight where a balance changes happened
    async fn get_last_topoheight_for_balance(&self, key: &PublicKey, asset: &Hash) -> Result<TopoHeight, BlockchainError> {
        trace!("get last topoheight for balance {} for {}", asset, key.as_address(self.is_mainnet()));
        let key = self.get_balance_key_for(key, asset);
        if !self.has_balance_internal(&key).await? {
            return Ok(0)
        }

        self.get_cacheable_data(&self.balances, &None, &key, DiskContext::LastTopoHeightForBalance).await
    }

    // set in storage the new top topoheight (the most up-to-date versioned balance)
    fn set_last_topoheight_for_balance(&mut self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> Result<(), BlockchainError> {
        trace!("set last topoheight to {} for balance {} for {}", topoheight, asset, key.as_address(self.is_mainnet()));
        let key = self.get_balance_key_for(key, asset);
        Self::insert_into_disk(self.snapshot.as_mut(), &self.balances, &key, &topoheight.to_be_bytes())?;
        Ok(())
    }

    // get the balance at a specific topoheight
    // if there is no balance change at this topoheight just return an error
    async fn has_balance_at_exact_topoheight(&self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> Result<bool, BlockchainError> {
        trace!("has balance {} for {} at exact topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
        // check first that this address has balance, if no returns
        if !self.has_balance_for(key, asset).await? {
            return Ok(false)
        }

        let key = self.get_versioned_balance_key(key, asset, topoheight);
        self.contains_data(&self.versioned_balances, &key)
    }

    // get the balance at a specific topoheight
    // if there is no balance change at this topoheight just return an error
    async fn get_balance_at_exact_topoheight(&self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> Result<VersionedBalance, BlockchainError> {
        trace!("get balance {} for {} at exact topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
        // check first that this address has balance, if no returns
        if !self.has_balance_at_exact_topoheight(key, asset, topoheight).await? {
            trace!("No balance {} found for {} at exact topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
            return Err(BlockchainError::NoBalanceChanges(key.as_address(self.is_mainnet()), topoheight, asset.clone()))
        }

        let disk_key = self.get_versioned_balance_key(key, asset, topoheight);
        self.get_cacheable_data(&self.versioned_balances, &None, &disk_key, DiskContext::BalanceAtTopoHeight(topoheight)).await
            .map_err(|_| BlockchainError::NoBalanceChanges(key.as_address(self.is_mainnet()), topoheight, asset.clone()))
    }

    // get the latest balance at maximum specified topoheight
    // when a DAG re-ordering happens, we need to select the right balance and not the last one
    // returns None if the key has no balances for this asset
    // Maximum topoheight is inclusive
    async fn get_balance_at_maximum_topoheight(&self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> Result<Option<(TopoHeight, VersionedBalance)>, BlockchainError> {
        trace!("get balance {} for {} at maximum topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
        // check first that this address has balance for this asset, if no returns None
        if !self.has_balance_for(key, asset).await? {
            trace!("No balance {} found for {} at maximum topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
            return Ok(None)
        }

        let topo = if self.has_balance_at_exact_topoheight(key, asset, topoheight).await? {
            topoheight
        } else {
            self.get_last_topoheight_for_balance(key, asset).await?
        };

        let mut previous_topoheight = Some(topo);
        // otherwise, we have to go through the whole chain
        while let Some(topo) = previous_topoheight {
            if topo <= topoheight {
                let version = self.get_balance_at_exact_topoheight(key, asset, topo).await?;
                return Ok(Some((topo, version)))
            }

            previous_topoheight = self.load_from_disk(&self.versioned_balances, &self.get_versioned_balance_key(key, asset, topo), DiskContext::BalanceAtTopoHeight(topo))?;
        }

        Ok(None)
    }

    // returns a new versioned balance with already-set previous topoheight
    // Topoheight is the new topoheight for the versioned balance,
    // We create a new versioned balance by taking the previous version and setting it as previous topoheight
    async fn get_new_versioned_balance(&self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> Result<(VersionedBalance, bool), BlockchainError> {
        trace!("get new versioned balance {} for {} at {}", asset, key.as_address(self.is_mainnet()), topoheight);

        match self.get_balance_at_maximum_topoheight(key, asset, topoheight).await? {
            Some((topo, mut version)) => {
                trace!("new versioned balance (balance at maximum topoheight) topo: {}, previous: {:?}, requested topo: {}", topo, version.get_previous_topoheight(), topo);
                // Mark it as clean
                version.prepare_new(Some(topo));
                Ok((version, false))
            },
            // if its the first balance, then we return a zero balance
            None => Ok((VersionedBalance::zero(), true))
        }
    }

    async fn get_output_balance_at_maximum_topoheight(&self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> Result<Option<(TopoHeight, VersionedBalance)>, BlockchainError> {
        trace!("get output balance {} for {} at maximum topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
        if !self.has_balance_for(key, asset).await? {
            trace!("No balance {} found for {} at maximum topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
            return Ok(None)
        }

        let topo = if self.has_balance_at_exact_topoheight(key, asset, topoheight).await? {
            topoheight
        } else {
            self.get_last_topoheight_for_balance(key, asset).await?
        };

        let mut next = Some(topo);
        while let Some(topo) = next {
            // We read the next topoheight (previous topo of the versioned balance) and its current balance type
            let (prev_topo, balance_type): (Option<u64>, BalanceType) = self.load_from_disk(&self.versioned_balances, &self.get_versioned_balance_key(key, asset, topo), DiskContext::BalanceAtTopoHeight(topo))?;
            if topo <= topoheight && balance_type.contains_output() {
                let version = self.get_balance_at_exact_topoheight(key, asset, topo).await?;
                return Ok(Some((topo, version)))
            }

            next = prev_topo;
        }

        Ok(None)
    }

    async fn get_output_balance_in_range(&self, key: &PublicKey, asset: &Hash, min_topoheight: TopoHeight, max_topoheight: TopoHeight) -> Result<Option<(TopoHeight, VersionedBalance)>, BlockchainError> {
        trace!("get output balance {} for {} in range {} - {}", asset, key.as_address(self.is_mainnet()), min_topoheight, max_topoheight);
        if !self.has_balance_for(key, asset).await? {
            trace!("No balance {} found for {} in range {} - {}", asset, key.as_address(self.is_mainnet()), min_topoheight, max_topoheight);
            return Ok(None)
        }

        let topo = if self.has_balance_at_exact_topoheight(key, asset, max_topoheight).await? {
            max_topoheight
        } else {
            self.get_last_topoheight_for_balance(key, asset).await?
        };

        let mut next = Some(topo);
        while let Some(topo) = next {
            if topo < min_topoheight {
                break;
            }

            // We read the next topoheight (previous topo of the versioned balance) and its current balance type
            let (prev_topo, balance_type): (Option<u64>, BalanceType) = self.load_from_disk(&self.versioned_balances, &self.get_versioned_balance_key(key, asset, topo), DiskContext::BalanceAtTopoHeight(topo))?;
            if topo <= max_topoheight && balance_type.contains_output() {
                let version = self.get_balance_at_exact_topoheight(key, asset, topo).await?;
                return Ok(Some((topo, version)))
            }

            next = prev_topo;
        }

        Ok(None)
    }

    // save a new versioned balance in storage and update the pointer
    async fn set_last_balance_to(&mut self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight, version: &VersionedBalance) -> Result<(), BlockchainError> {
        trace!("set balance {} for {} to topoheight {}", asset, key.as_address(self.is_mainnet()), topoheight);
        self.set_balance_at_topoheight(asset, topoheight, key, &version).await?;
        self.set_last_topoheight_for_balance(key, asset, topoheight)?;
        Ok(())
    }

    // get the last version of balance and returns topoheight
    async fn get_last_balance(&self, key: &PublicKey, asset: &Hash) -> Result<(TopoHeight, VersionedBalance), BlockchainError> {
        trace!("get last balance {} for {}", asset, key.as_address(self.is_mainnet()));
        if !self.has_balance_for(key, asset).await? {
            trace!("No balance {} found for {}", asset, key.as_address(self.is_mainnet()));
            return Err(BlockchainError::NoBalance(key.as_address(self.is_mainnet())))
        }

        let topoheight = self.get_cacheable_data(&self.balances, &None, &self.get_balance_key_for(key, asset), DiskContext::LastBalance).await?;
        let version = self.get_balance_at_exact_topoheight(key, asset, topoheight).await?;
        Ok((topoheight, version))
    }

    // save the asset balance at specific topoheight
    async fn set_balance_at_topoheight(&mut self, asset: &Hash, topoheight: TopoHeight, key: &PublicKey, balance: &VersionedBalance) -> Result<(), BlockchainError> {
        trace!("set balance {} at topoheight {} for {}", asset, topoheight, key.as_address(self.is_mainnet()));
        let key = self.get_versioned_balance_key(key, asset, topoheight);
        Self::insert_into_disk(self.snapshot.as_mut(), &self.versioned_balances, &key, balance.to_bytes())?;

        Ok(())
    }

    async fn get_account_summary_for(&self, key: &PublicKey, asset: &Hash, min_topoheight: TopoHeight, max_topoheight: TopoHeight) -> Result<Option<AccountSummary>, BlockchainError> {
        trace!("get account summary {} for {} at maximum topoheight {}", asset, key.as_address(self.is_mainnet()), max_topoheight);

        // first search if we have a valid balance at the maximum topoheight
        if let Some((topo, version)) = self.get_balance_at_maximum_topoheight(key, asset, max_topoheight).await? {
            if topo < min_topoheight {
                trace!("No changes found for {} above min topoheight {}", key.as_address(self.is_mainnet()), min_topoheight);
                return Ok(None)
            }

            
            let mut account = AccountSummary {
                output_topoheight: None,
                stable_topoheight: topo,
            };
            
            // We have an output in it, we can return the account
            if version.contains_output() {
                trace!("Stable with output balance found for {} at topoheight {}", key.as_address(self.is_mainnet()), topo);
                return Ok(Some(account))
            }

            // We need to search through the whole history to see if we have a balance with output
            let mut previous = version.get_previous_topoheight();
            while let Some(topo) = previous {
                let previous_version = self.get_balance_at_exact_topoheight(key, asset, topo).await?;
                if previous_version.contains_output() {
                    trace!("Output balance found for {} at topoheight {}", key.as_address(self.is_mainnet()), topo);
                    account.output_topoheight = Some(topo);
                    break;
                }

                previous = previous_version.get_previous_topoheight();
            }

            return Ok(Some(account))
        }

        trace!("No balance found for {} at maximum topoheight {}", key.as_address(self.is_mainnet()), max_topoheight);
        Ok(None)
    }

    async fn get_spendable_balances_for(&self, key: &PublicKey, asset: &Hash, min_topoheight: TopoHeight, max_topoheight: TopoHeight, maximum: usize) -> Result<(Vec<Balance>, Option<TopoHeight>), BlockchainError> {
        trace!("get spendable balances for {} at maximum topoheight {}", key.as_address(self.is_mainnet()), max_topoheight);

        let mut balances = Vec::new();

        let mut fetch_topoheight = Some(max_topoheight);
        while let Some(topo) = fetch_topoheight.take().filter(|&t| t >= min_topoheight && balances.len() < maximum) {
            let version = self.get_balance_at_exact_topoheight(key, asset, topo).await?;
            let has_output = version.contains_output();
            let previous_topoheight = version.get_previous_topoheight();
            balances.push(version.as_balance(topo));

            if has_output {
                trace!("Output balance found for {} at topoheight {}", key.as_address(self.is_mainnet()), topo);
                break;
            } else {
                fetch_topoheight = previous_topoheight;
            }
        }

        trace!("balances {} {}, {} - {}", balances.len(), key.as_address(self.is_mainnet()), min_topoheight, max_topoheight);
        Ok((balances, fetch_topoheight))
    }
}