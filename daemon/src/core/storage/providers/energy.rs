use async_trait::async_trait;
use terminos_common::{
    account::EnergyResource,
    crypto::PublicKey,
    block::TopoHeight,
};
use crate::core::error::BlockchainError;

/// Provider for energy resource storage operations
#[async_trait]
pub trait EnergyProvider {
    /// Get energy resource for an account
    async fn get_energy_resource(&self, account: &PublicKey) -> Result<Option<EnergyResource>, BlockchainError>;

    /// Set energy resource for an account at a specific topoheight
    async fn set_energy_resource(&mut self, account: &PublicKey, topoheight: TopoHeight, energy: &EnergyResource) -> Result<(), BlockchainError>;
} 