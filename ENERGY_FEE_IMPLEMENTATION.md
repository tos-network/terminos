# Terminos Energy Fee Implementation Guide

## Overview

This document details how to implement energy-based fees in Terminos, where **energy can only be used for Transfer transactions** to provide users with the opportunity to stake TOS for free transfers of TOS and other tokens. The system includes TRON-style freeze durations and reward multipliers.

## Key Design Principle

**Energy is exclusively for Transfer transactions**: Unlike the previous implementation that allowed energy for all transaction types, the new system restricts energy usage to Transfer transactions only. This provides users with a clear incentive to stake TOS for free transfers while maintaining TOS fees for other operations like contract deployment and calls.

**TRON-style Freeze System**: Users can freeze TOS for different durations (3, 7, or 14 days) with corresponding reward multipliers (1.0x, 1.1x, 1.2x) to encourage long-term staking.

## Current Implementation Status

The code has been updated to implement the following changes:

### 1. Freeze Duration and Reward System

#### 1.1 FreezeDuration Enum
```rust
/// Freeze duration options for TOS staking (inspired by TRON's freeze mechanism)
/// Different durations provide different reward multipliers to encourage long-term staking
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FreezeDuration {
    /// 3-day freeze period (1.0x reward multiplier)
    Day3,
    /// 7-day freeze period (1.1x reward multiplier)  
    Day7,
    /// 14-day freeze period (1.2x reward multiplier)
    Day14,
}

impl FreezeDuration {
    /// Get the reward multiplier for this freeze duration
    /// Inspired by TRON's freeze reward system
    pub fn reward_multiplier(&self) -> f64 {
        match self {
            FreezeDuration::Day3 => 1.0,
            FreezeDuration::Day7 => 1.1,
            FreezeDuration::Day14 => 1.2,
        }
    }

    /// Get the duration in blocks (assuming 1 block per second)
    pub fn duration_in_blocks(&self) -> u64 {
        match self {
            FreezeDuration::Day3 => 3 * 24 * 60 * 60,  // 3 days in seconds
            FreezeDuration::Day7 => 7 * 24 * 60 * 60,  // 7 days in seconds
            FreezeDuration::Day14 => 14 * 24 * 60 * 60, // 14 days in seconds
        }
    }
}
```

#### 1.2 FreezeRecord Structure
```rust
/// Individual freeze record tracking a specific freeze operation
/// Each freeze operation creates a separate record with its own duration and unlock time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreezeRecord {
    /// Amount of TOS frozen in this record
    pub amount: u64,
    /// Freeze duration chosen for this record
    pub duration: FreezeDuration,
    /// Topoheight when the freeze was initiated
    pub freeze_topoheight: TopoHeight,
    /// Topoheight when this freeze can be unlocked
    pub unlock_topoheight: TopoHeight,
    /// Energy gained from this freeze (with multiplier applied)
    pub energy_gained: u64,
}
```

### 2. Transaction Verification Logic

#### 2.1 Modified `uses_energy_for_fees()` method
```rust
/// Check if this transaction uses energy for fees
/// Energy can only be used for Transfer transactions to provide free TOS and other token transfers
pub fn uses_energy_for_fees(&self) -> bool {
    // Energy can only be used for Transfer transactions
    // This provides users with the opportunity to stake TOS for free transfers
    self.fee == 0 && matches!(self.get_data(), TransactionType::Transfers(_))
}
```

#### 2.2 Updated `calculate_energy_cost()` method
```rust
/// Calculate energy cost for this transaction
/// Energy can only be used for Transfer transactions, so this method focuses on transfer-specific costs
pub fn calculate_energy_cost(&self) -> u64 {
    // Energy can only be used for Transfer transactions
    // Calculate energy cost based on transfer-specific parameters
    calculate_energy_fee(
        self.size(),
        self.get_outputs_count(),
        0 // new_addresses will be calculated during verification
    )
}
```

### 3. Transaction Application Logic

#### 3.1 Updated energy consumption in `apply()` function
```rust
// Handle energy consumption for transfer transactions only
// Energy provides users with the opportunity to stake TOS for free transfers
if self.uses_energy_for_fees() {
    let energy_cost = self.calculate_energy_cost();
    
    // Get user's energy resource
    let energy_resource = state.get_energy_resource(&self.source).await
        .map_err(VerificationError::State)?;
    
    // Check if user has enough energy for the transfer
    if !energy_resource.has_enough_energy(energy_cost) {
        return Err(VerificationError::InsufficientEnergy(energy_cost));
    }
    
    // Consume energy for the transfer transaction
    let mut energy_resource = energy_resource;
    energy_resource.consume_energy(energy_cost)
        .map_err(|_| VerificationError::InsufficientEnergy(energy_cost))?;
    
    // Update energy resource in state
    state.update_energy_resource(&self.source, energy_resource).await
        .map_err(VerificationError::State)?;
    
    debug!("Consumed {} energy for transfer transaction {}", energy_cost, tx_hash);
}
```

### 4. Energy Transaction Payload

#### 4.1 EnergyPayload with Duration Support
```rust
/// Energy-related transaction payloads for Transfer operations only
/// Enhanced with TRON-style freeze duration and reward multiplier system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnergyPayload {
    /// Freeze TOS to get energy for free transfers with duration-based rewards
    FreezeTos {
        /// Amount of TOS to freeze
        amount: u64,
        /// Freeze duration (3, 7, or 14 days) affecting reward multiplier
        duration: FreezeDuration,
    },
    /// Unfreeze TOS (release frozen TOS) - can only unfreeze after lock period
    UnfreezeTos {
        /// Amount of TOS to unfreeze
        amount: u64,
    },
}
```

### 5. Transaction Builder Updates

#### 5.1 EnergyBuilder with Duration Support
```rust
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
}
```

### 6. Wallet CLI Integration

#### 6.1 Freeze TOS Command with Duration Selection
```rust
// Read freeze duration
let duration = if args.has_argument("duration") {
    let duration_str = args.get_value("duration")?.to_string_value()?;
    match duration_str.as_str() {
        "3" | "3d" | "3days" => terminos_common::account::energy::FreezeDuration::Day3,
        "7" | "7d" | "7days" => terminos_common::account::energy::FreezeDuration::Day7,
        "14" | "14d" | "14days" => terminos_common::account::energy::FreezeDuration::Day14,
        _ => {
            manager.error("Invalid duration. Please choose: 3, 7, or 14 days");
            return Ok(())
        }
    }
} else {
    // Show duration options and let user choose
    manager.message("Choose freeze duration:");
    manager.message("  1. 3 days  (1.0x reward multiplier)");
    manager.message("  2. 7 days  (1.1x reward multiplier)");
    manager.message("  3. 14 days (1.2x reward multiplier)");
    
    let choice: String = prompt.read(
        prompt.colorize_string(Color::Green, "Enter choice (1-3): ")
    ).await.context("Error while reading duration choice")?;
    
    match choice.as_str() {
        "1" => terminos_common::account::energy::FreezeDuration::Day3,
        "2" => terminos_common::account::energy::FreezeDuration::Day7,
        "3" => terminos_common::account::energy::FreezeDuration::Day14,
        _ => {
            manager.error("Invalid choice. Please enter 1, 2, or 3");
            return Ok(())
        }
    }
};

// Calculate energy gain
let energy_gain = (amount as f64 * duration.reward_multiplier()) as u64;

manager.message(format!("Freezing {} TOS for {} days", format_coin(amount, asset_data.get_decimals()), duration.name()));
manager.message(format!("Reward multiplier: {}x", duration.reward_multiplier()));
manager.message(format!("Energy gained: {} units", energy_gain));
```

## Usage Examples

### 1. Creating Transfer Transactions with Energy Fees

```rust
// Build a transfer transaction using energy for fees
let tx = TransactionBuilder::new(version, source_key, None, tx_type, FeeBuilder::default())
    .with_energy_fees()  // Use energy for transfer fees
    .build(state, keypair)?;
```

### 2. Freezing TOS for Energy with Duration

```rust
// Freeze 1000 TOS for 7 days (1.1x multiplier)
let energy_builder = EnergyBuilder::freeze_tos(1000, FreezeDuration::Day7);
let tx_type = TransactionTypeBuilder::Energy(energy_builder);

let tx = TransactionBuilder::new(version, source_key, None, tx_type, FeeBuilder::default())
    .build(state, keypair)?;
```

### 3. Creating Non-Transfer Transactions

```rust
// Contract deployment and calls always use TOS fees
let tx = TransactionBuilder::new(version, source_key, None, contract_tx_type, FeeBuilder::default())
    .with_tos_fees(estimated_fee)  // TOS fees required for contracts
    .build(state, keypair)?;
```

## Benefits of This Implementation

### 1. Clear Incentive Structure
- **Transfer transactions**: Can use energy for free transfers
- **Contract operations**: Must use TOS fees
- **Other operations**: Must use TOS fees
- **Long-term staking**: Higher reward multipliers for longer freeze periods

### 2. User-Friendly
- Users can stake TOS to get energy for free transfers
- Clear distinction between free (energy) and paid (TOS) operations
- Interactive duration selection with reward multiplier display
- No confusion about which operations can use energy

### 3. Economic Benefits
- Encourages TOS staking for energy generation
- Rewards long-term staking with higher multipliers
- Maintains TOS utility for contract operations
- Prevents energy spam on expensive operations

### 4. Technical Benefits
- Simplified logic: energy only for transfers
- Clear separation of concerns
- TRON-style freeze duration system
- Easier to understand and maintain

## Testing Strategy

### Unit Tests
```rust
#[test]
fn test_freeze_duration_reward_multipliers() {
    assert_eq!(FreezeDuration::Day3.reward_multiplier(), 1.0);
    assert_eq!(FreezeDuration::Day7.reward_multiplier(), 1.1);
    assert_eq!(FreezeDuration::Day14.reward_multiplier(), 1.2);
}

#[test]
fn test_energy_only_for_transfers() {
    let transfer_tx = create_transfer_transaction();
    let contract_tx = create_contract_transaction();
    
    assert!(transfer_tx.uses_energy_for_fees());  // Should work
    assert!(!contract_tx.uses_energy_for_fees()); // Should not work
}
```

### Integration Tests
- Test energy consumption for transfer transactions
- Test TOS fee requirements for contract operations
- Test freeze duration and reward multiplier system
- Test unfreeze logic with lock periods

## Conclusion

This implementation successfully restricts energy usage to transfer transactions only while adding TRON-style freeze duration and reward multiplier system. It provides users with a clear and valuable incentive to stake TOS for free transfers while maintaining the economic model for other operations. The code changes are comprehensive and maintain backward compatibility while introducing the new energy-based fee system with duration-based rewards. 