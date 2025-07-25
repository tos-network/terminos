mod data;
mod output;
mod balance;
mod supply;
mod r#impl;

use async_trait::async_trait;
use log::trace;
use terminos_common::{
    account::CiphertextCache,
    asset::AssetData,
    block::TopoHeight,
    contract::{ContractProvider as ContractAccess, ContractStorage},
    crypto::{Hash, PublicKey},
    tokio::try_block_on
};
use terminos_vm::ValueCell;
use crate::core::storage::*;

#[async_trait]
impl ContractAccess for RocksStorage {
    fn get_contract_balance_for_asset(&self, contract: &Hash, asset: &Hash, topoheight: TopoHeight) -> Result<Option<(TopoHeight, u64)>, anyhow::Error> {
        trace!("get contract balance for contract {} asset {}", contract, asset);
        let res = try_block_on(self.get_contract_balance_at_maximum_topoheight(contract, asset, topoheight))??;
        Ok(res.map(|(topoheight, balance)| (topoheight, balance.take())))
    }

    fn asset_exists(&self, asset: &Hash, topoheight: TopoHeight) -> Result<bool, anyhow::Error> {
        trace!("check if asset {} exists at topoheight {}", asset, topoheight);
        let contains = try_block_on(self.is_asset_registered_at_maximum_topoheight(asset, topoheight))??;
        Ok(contains)
    }

    fn account_exists(&self, key: &PublicKey, topoheight: TopoHeight) -> Result<bool, anyhow::Error> {
        trace!("check if account {} exists at topoheight {}", key.as_address(self.is_mainnet()), topoheight);

        let contains = try_block_on(self.is_account_registered_for_topoheight(key, topoheight))??;
        Ok(contains)
    }

    // Load the asset data from the storage
    fn load_asset_data(&self, asset: &Hash, topoheight: TopoHeight) -> Result<Option<(TopoHeight, AssetData)>, anyhow::Error> {
        trace!("load asset data for asset {} at topoheight {}", asset, topoheight);
        let res = try_block_on(self.get_asset_at_maximum_topoheight(asset, topoheight))??;
        Ok(res.map(|(topo, v)| (topo, v.take())))
    }

    fn load_asset_supply(&self, asset: &Hash, topoheight: TopoHeight) -> Result<Option<(TopoHeight, u64)>, anyhow::Error> {
        trace!("load asset supply for asset {} at topoheight {}", asset, topoheight);
        let res = try_block_on(self.get_asset_supply_at_maximum_topoheight(asset, topoheight))??;
        Ok(res.map(|(topoheight, supply)| (topoheight, supply.take())))
    }

    fn get_account_balance_for_asset(&self, key: &PublicKey, asset: &Hash, topoheight: TopoHeight) -> Result<Option<(TopoHeight, CiphertextCache)>, anyhow::Error> {
        trace!("get account {} balance for asset {} at topoheight {}", key.as_address(self.is_mainnet()), asset, topoheight);
        let res = try_block_on(self.get_balance_at_maximum_topoheight(key, asset, topoheight))??;
        Ok(res.map(|(topoheight, balance)| (topoheight, balance.take_balance())))
    }
}

impl ContractStorage for RocksStorage {
    fn load_data(&self, contract: &Hash, key: &ValueCell, topoheight: TopoHeight) -> Result<Option<(TopoHeight, Option<ValueCell>)>, anyhow::Error> {
        trace!("load contract {} key {} data at topoheight {}", contract, key, topoheight);
        let res = try_block_on(self.get_contract_data_at_maximum_topoheight_for(contract, &key, topoheight))??;

        match res {
            Some((topoheight, data)) => match data.take() {
                Some(data) => Ok(Some((topoheight, Some(data)))),
                None => Ok(Some((topoheight, None))),
            },
            None => Ok(None),
        }
    }

    fn has_data(&self, contract: &Hash, key: &ValueCell, topoheight: TopoHeight) -> Result<bool, anyhow::Error> {
        trace!("check if contract {} key {} data exists at topoheight {}", contract, key, topoheight);
        let contains = try_block_on(self.has_contract_data_at_maximum_topoheight(contract, &key, topoheight))??;
        Ok(contains)
    }

    fn load_data_latest_topoheight(&self, contract: &Hash, key: &ValueCell, topoheight: TopoHeight) -> Result<Option<TopoHeight>, anyhow::Error> {
        trace!("load data latest topoheight for contract {} key {} at topoheight {}", contract, key, topoheight);
        let res = try_block_on(self.get_contract_data_topoheight_at_maximum_topoheight_for(contract, &key, topoheight))??;
        Ok(res)
    }

    fn has_contract(&self, contract: &Hash, topoheight: TopoHeight) -> Result<bool, anyhow::Error> {
        trace!("has contract {} at topoheight {}", contract, topoheight);
        let res = try_block_on(self.has_contract_at_maximum_topoheight(contract, topoheight))??;
        Ok(res)
    }
}