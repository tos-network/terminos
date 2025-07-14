# Terminos Energy-Based Fee Model

## Overview

Terminos adopts an optimized TRON-style fee model, using **Energy** as the sole resource unit and removing the concept of Bandwidth. This greatly simplifies resource management. The system includes TRON-style freeze durations and reward multipliers to encourage long-term staking.

## Key Features

### 1. Unified Resource System
- **Energy Only:** All operations consume energy.
- **Flexible Acquisition:** Obtain energy by freezing TOS with duration-based rewards, leasing, or direct purchase.

### 2. TRON-Style Freeze System
- **Freeze Durations:** 3, 7, or 14 days with corresponding reward multipliers
- **Reward Multipliers:** 1.0x (3 days), 1.1x (7 days), 1.2x (14 days)
- **Lock Periods:** TOS must remain frozen for the chosen duration before unfreezing

### 3. Account Activation
- **Activation Fee:** 0.1 TOS (similar to TRON's 0.1 TRX).
- **One-Time Fee:** Once activated, the account is always active.
- **Free Activation:** Accounts are auto-activated when receiving block rewards from mining.

## Fee Structure

### Basic Operation Costs

| Operation         | Energy Cost | TOS Equivalent | Description                  |
|-------------------|-------------|----------------|------------------------------|
| Basic Transfer    | 1 energy    | 0.0001 TOS     | Per transfer                 |
| Per KB Data       | 10 energy   | 0.001 TOS      | Per KB of transaction data   |
| Contract Deploy   | 1000 energy | 0.1 TOS        | Smart contract deployment    |
| Contract Call     | 100 energy  | 0.01 TOS       | Smart contract invocation    |
| Storage Operation | 10 energy   | 0.001 TOS      | Data storage                 |
| Per Byte Storage  | 1 energy    | 0.0001 TOS     | Per byte stored              |
| Multisig Sign     | 5 energy    | 0.0005 TOS     | Per multisig signature       |

### How to Obtain Energy

#### 1. Freeze TOS for Energy with Duration-Based Rewards
```rust
// Freeze TOS with different durations and reward multipliers
let energy_gained_3d = energy_resource.freeze_tos_for_energy(100000000, FreezeDuration::Day3, topoheight);
// Freezing 1 TOS for 3 days gives 1.0 energy (1:1 ratio)

let energy_gained_7d = energy_resource.freeze_tos_for_energy(100000000, FreezeDuration::Day7, topoheight);
// Freezing 1 TOS for 7 days gives 1.1 energy (1.1x multiplier)

let energy_gained_14d = energy_resource.freeze_tos_for_energy(100000000, FreezeDuration::Day14, topoheight);
// Freezing 1 TOS for 14 days gives 1.2 energy (1.2x multiplier)
```

#### 2. Energy Leasing
```rust
// Create a lease contract
let lease = EnergyLease::new(
    lessor,      // Lessor
    lessee,      // Lessee
    1000,        // Amount of energy
    1000,        // Lease duration (blocks)
    topoheight,  // Start block
    1000         // Price per energy unit
);
```

#### 3. Direct TOS Consumption
```rust
// If energy is insufficient, buy at market rate
// 1 energy = 0.0001 TOS
let tos_cost = energy_needed * ENERGY_TO_TOS_RATE;
```

## Freeze Duration System

### Duration Options and Rewards

| Duration | Reward Multiplier | Energy Gain | Lock Period | Use Case |
|----------|-------------------|-------------|-------------|----------|
| 3 days   | 1.0x             | 1:1 ratio   | 3 days      | Short-term staking |
| 7 days   | 1.1x             | 1.1:1 ratio | 7 days      | Medium-term staking |
| 14 days  | 1.2x             | 1.2:1 ratio | 14 days     | Long-term staking |

### Freeze Record Management
```rust
/// Individual freeze record tracking a specific freeze operation
pub struct FreezeRecord {
    pub amount: u64,                    // Amount of TOS frozen
    pub duration: FreezeDuration,       // 3, 7, or 14 days
    pub freeze_topoheight: TopoHeight,  // When freeze started
    pub unlock_topoheight: TopoHeight,  // When can be unlocked
    pub energy_gained: u64,             // Energy gained (with multiplier)
}

impl FreezeRecord {
    /// Check if this freeze record can be unlocked
    pub fn can_unlock(&self, current_topoheight: TopoHeight) -> bool {
        current_topoheight >= self.unlock_topoheight
    }
    
    /// Get remaining lock time in blocks
    pub fn remaining_blocks(&self, current_topoheight: TopoHeight) -> u64 {
        if current_topoheight >= self.unlock_topoheight {
            0
        } else {
            self.unlock_topoheight - current_topoheight
        }
    }
}
```

## Architecture

### Core Modules

1. **`common/src/account/energy.rs`**
   - Energy resource management
   - Freeze/unfreeze logic with duration support
   - Energy consumption
   - FreezeRecord tracking

2. **`common/src/utils/energy_fee.rs`**
   - Energy fee calculation
   - Resource manager
   - Status monitoring

3. **`common/src/transaction/payload/energy.rs`**
   - Energy-related transaction types
   - Freeze duration support in payloads
   - Transaction payload definitions

### Configuration Constants

```rust
// Account activation fee
pub const ACCOUNT_ACTIVATION_FEE: u64 = 10000000; // 0.1 TOS

// Energy cost standards
pub const ENERGY_PER_TRANSFER: u64 = 1;           // Basic transfer
pub const ENERGY_PER_KB: u64 = 10;                // Per KB data
pub const ENERGY_PER_CONTRACT_DEPLOY: u64 = 1000; // Contract deploy
pub const ENERGY_PER_CONTRACT_CALL: u64 = 100;    // Contract call
pub const ENERGY_PER_STORE_OP: u64 = 10;          // Storage operation
pub const ENERGY_PER_BYTE_STORED: u64 = 1;        // Per byte stored
pub const ENERGY_PER_MULTISIG_SIGNATURE: u64 = 5; // Multisig signature

// Energy to TOS rate
pub const ENERGY_TO_TOS_RATE: u64 = 10000; // 0.0001 TOS per energy

// Freeze duration constants (in seconds)
pub const FREEZE_DURATION_3_DAYS: u64 = 3 * 24 * 60 * 60;
pub const FREEZE_DURATION_7_DAYS: u64 = 7 * 24 * 60 * 60;
pub const FREEZE_DURATION_14_DAYS: u64 = 14 * 24 * 60 * 60;
```

## Usage Examples

### Calculating Transaction Fees

```rust
use terminos_common::utils::energy_fee::EnergyFeeCalculator;

// Calculate energy cost
let energy_cost = EnergyFeeCalculator::calculate_energy_cost(
    1024,    // Transaction size (bytes)
    1,       // Output count
    0,       // New address count
    0,       // Multisig count
    false,   // Is contract deploy
    false    // Is contract call
);

// Calculate total cost
let (energy_consumed, tos_cost) = EnergyFeeCalculator::calculate_total_cost(
    energy_cost,
    0,  // New address count
    &energy_resource
);
```

### Managing Energy Resources with Duration-Based Freezing

```rust
use terminos_common::account::energy::{EnergyResourceManager, FreezeDuration};

// Create energy resource
let mut energy_resource = EnergyResourceManager::create_energy_resource();

// Freeze TOS for energy with different durations
let energy_gained_3d = EnergyResourceManager::freeze_tos_for_energy(
    &mut energy_resource,
    100000000, // 1 TOS
    FreezeDuration::Day3,
    1000
);
assert_eq!(energy_gained_3d, 100000000); // 1.0x multiplier

let energy_gained_7d = EnergyResourceManager::freeze_tos_for_energy(
    &mut energy_resource,
    100000000, // 1 TOS
    FreezeDuration::Day7,
    1000
);
assert_eq!(energy_gained_7d, 110000000); // 1.1x multiplier

let energy_gained_14d = EnergyResourceManager::freeze_tos_for_energy(
    &mut energy_resource,
    100000000, // 1 TOS
    FreezeDuration::Day14,
    1000
);
assert_eq!(energy_gained_14d, 120000000); // 1.2x multiplier

// Consume energy
EnergyResourceManager::consume_energy_for_transaction(
    &mut energy_resource,
    energy_cost
)?;

// Get energy status
let status = EnergyResourceManager::get_energy_status(&energy_resource);
println!("Available energy: {}", status.available_energy);

// Check unlockable TOS
let unlockable_tos = EnergyResourceManager::get_unlockable_tos(&energy_resource, current_topoheight);
println!("Unlockable TOS: {}", unlockable_tos);
```

### Wallet CLI Usage

```bash
# Freeze TOS with duration selection
freeze_tos 1000 7d  # Freeze 1000 TOS for 7 days (1.1x multiplier)

# Or interactive mode
freeze_tos
# Choose freeze duration:
#   1. 3 days  (1.0x reward multiplier)
#   2. 7 days  (1.1x reward multiplier)
#   3. 14 days (1.2x reward multiplier)

# Check energy status
energy

# Unfreeze TOS (only after lock period expires)
unfreeze_tos 500
```

## Advantages

### 1. Simplicity
- Unified resource system, reduced complexity
- Clear fee structure
- Easy to understand and predict

### 2. Flexibility
- Multiple ways to obtain energy
- Duration-based reward system encourages long-term staking
- Supports energy leasing market
- Automatic TOS conversion

### 3. Economic Efficiency
- Reasonable fee standards
- Prevents spam transactions
- Supports high-frequency transactions
- Rewards long-term staking with higher multipliers

### 4. Extensibility
- Modular design
- Easy to adjust parameters
- Ready for future feature expansion
- TRON-style freeze system proven in production

## Comparison with TRON

| Feature         | TRON                | Terminos         |
|-----------------|---------------------|------------------|
| Resource Types  | Energy + Bandwidth  | Energy only      |
| Account Activation | 0.1 TRX          | 0.1 TOS          |
| Basic Transfer  | Consumes Bandwidth  | Consumes Energy  |
| Contract Ops    | Consumes Energy     | Consumes Energy  |
| Resource Acquire| Freeze TRX          | Freeze TOS       |
| Freeze Duration | Fixed periods       | 3/7/14 days      |
| Reward Multiplier| Fixed 1:1         | 1.0x/1.1x/1.2x   |
| Leasing Market  | Supported           | Supported        |

## Summary

The Terminos energy-based fee model retains the economic advantages of TRON while further simplifying resource management by removing bandwidth. The addition of TRON-style freeze durations and reward multipliers provides users with clear incentives for long-term staking while maintaining flexibility for different use cases. This provides a clearer and more flexible fee mechanism, ensuring network security and offering users multiple ways to obtain resources—laying a solid foundation for the long-term development of Terminos. 