use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use terminos_vm::ValueCell;
use crate::{
    api::DataElement,
    crypto::{Address, Hash},
    account::energy::FreezeDuration,
};

fn default_bool_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransferBuilder {
    pub asset: Hash,
    pub amount: u64,
    pub destination: Address,
    // we can put whatever we want up to EXTRA_DATA_LIMIT_SIZE bytes
    pub extra_data: Option<DataElement>,
    // Encrypt the extra data by default
    // Set to false if you want to keep it public
    #[serde(default = "default_bool_true")]
    pub encrypt_extra_data: bool
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MultiSigBuilder {
    pub participants: IndexSet<Address>,
    pub threshold: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContractDepositBuilder {
    pub amount: u64,
    #[serde(default)]
    pub private: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InvokeContractBuilder {
    pub contract: Hash,
    pub max_gas: u64,
    pub chunk_id: u16,
    pub parameters: Vec<ValueCell>,
    #[serde(default)]
    pub deposits: IndexMap<Hash, ContractDepositBuilder>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeployContractBuilder {
    // Module to deploy
    pub module: String,
    // Inner invoke during the deploy
    pub invoke: Option<DeployContractInvokeBuilder>
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeployContractInvokeBuilder {
    pub max_gas: u64,
    #[serde(default)]
    pub deposits: IndexMap<Hash, ContractDepositBuilder>,
}

/// Energy transaction builder with TRON-style freeze duration support
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnergyBuilder {
    /// Amount of TOS to freeze or unfreeze
    pub amount: u64,
    /// Whether this is a freeze operation (true) or unfreeze operation (false)
    pub is_freeze: bool,
    /// Freeze duration for freeze operations (3, 7, or 14 days)
    /// This affects the reward multiplier: 1.0x, 1.1x, or 1.2x respectively
    /// Only used when is_freeze is true
    #[serde(default)]
    pub freeze_duration: Option<FreezeDuration>,
}

impl EnergyBuilder {
    /// Create a new freeze TOS builder with specified duration
    pub fn freeze_tos(amount: u64, duration: FreezeDuration) -> Self {
        Self {
            amount,
            is_freeze: true,
            freeze_duration: Some(duration),
        }
    }

    /// Create a new unfreeze TOS builder
    pub fn unfreeze_tos(amount: u64) -> Self {
        Self {
            amount,
            is_freeze: false,
            freeze_duration: None,
        }
    }

    /// Get the freeze duration for this operation
    pub fn get_duration(&self) -> Option<&FreezeDuration> {
        self.freeze_duration.as_ref()
    }

    /// Calculate the energy that would be gained from this freeze operation
    pub fn calculate_energy_gain(&self) -> Option<u64> {
        if self.is_freeze {
            self.freeze_duration.as_ref().map(|duration| {
                (self.amount as f64 * duration.reward_multiplier()) as u64
            })
        } else {
            None
        }
    }

    /// Validate the builder configuration
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.amount == 0 {
            return Err("Amount must be greater than 0");
        }

        if self.is_freeze {
            if self.freeze_duration.is_none() {
                return Err("Freeze duration must be specified for freeze operations");
            }
        } else {
            if self.freeze_duration.is_some() {
                return Err("Freeze duration should not be specified for unfreeze operations");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_energy_builder_freeze() {
        let builder = EnergyBuilder::freeze_tos(1000, FreezeDuration::Day7);
        
        assert_eq!(builder.amount, 1000);
        assert!(builder.is_freeze);
        assert_eq!(builder.get_duration(), Some(&FreezeDuration::Day7));
        assert_eq!(builder.calculate_energy_gain(), Some(1100)); // 1000 * 1.1
        assert!(builder.validate().is_ok());
    }

    #[test]
    fn test_energy_builder_unfreeze() {
        let builder = EnergyBuilder::unfreeze_tos(500);
        
        assert_eq!(builder.amount, 500);
        assert!(!builder.is_freeze);
        assert_eq!(builder.get_duration(), None);
        assert_eq!(builder.calculate_energy_gain(), None);
        assert!(builder.validate().is_ok());
    }

    #[test]
    fn test_energy_builder_validation() {
        // Test zero amount
        let builder = EnergyBuilder::freeze_tos(0, FreezeDuration::Day3);
        assert!(builder.validate().is_err());

        // Test freeze without duration
        let builder = EnergyBuilder {
            amount: 1000,
            is_freeze: true,
            freeze_duration: None,
        };
        assert!(builder.validate().is_err());

        // Test unfreeze with duration
        let builder = EnergyBuilder {
            amount: 1000,
            is_freeze: false,
            freeze_duration: Some(FreezeDuration::Day7),
        };
        assert!(builder.validate().is_err());
    }

    #[test]
    fn test_different_duration_rewards() {
        let amounts = [1000, 2000, 3000];
        let durations = [FreezeDuration::Day3, FreezeDuration::Day7, FreezeDuration::Day14];
        
        for amount in amounts {
            for duration in &durations {
                let builder = EnergyBuilder::freeze_tos(amount, duration.clone());
                let expected_energy = (amount as f64 * duration.reward_multiplier()) as u64;
                assert_eq!(builder.calculate_energy_gain(), Some(expected_energy));
            }
        }
    }
}