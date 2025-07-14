# Energy Deduction Logic Implementation in Apply Function

## Specific Location

Energy deduction logic should be added in the `apply` function in `common/src/transaction/verify/mod.rs`, **specifically as follows**:

### Function Structure
```rust
async fn apply<'a, P: ContractProvider, E, B: BlockchainApplyState<'a, P, E>>(
    &'a self,
    tx_hash: &'a Hash,
    state: &mut B,
    decompressed_deposits: &HashMap<&Hash, DecompressedDepositCt>,
) -> Result<(), VerificationError<E>> {
    trace!("Applying transaction data");
    
    // 1. Update nonce
    state.update_account_nonce(self.get_source(), self.nonce + 1).await
        .map_err(VerificationError::State)?;

    // 2. 🔥 ENERGY deduction logic should be added here 🔥
    // Location: After updating nonce, before applying receiver balances

    // 3. Apply receiver balances
    match &self.data {
        TransactionType::Transfers(transfers) => { /* ... */ },
        TransactionType::Burn(payload) => { /* ... */ },
        TransactionType::MultiSig(payload) => { /* ... */ },
        TransactionType::InvokeContract(payload) => { /* ... */ },
        TransactionType::DeployContract(payload) => { /* ... */ },
        TransactionType::Energy(_) => { /* ... */ }
    }

    Ok(())
}
```

## Specific Code Implementation

### Complete Energy Deduction Logic Code

```rust
// Handle energy consumption if this transaction uses energy for fees
if self.uses_energy_for_fees() {
    let energy_cost = self.calculate_energy_cost();
    
    // Get user's energy resource
    let energy_resource = state.get_energy_resource(&self.source).await
        .map_err(VerificationError::State)?;
    
    // Check if user has enough energy
    if !energy_resource.has_enough_energy(energy_cost) {
        return Err(VerificationError::InsufficientEnergy(energy_cost));
    }
    
    // Consume energy
    let mut energy_resource = energy_resource;
    energy_resource.consume_energy(energy_cost)
        .map_err(|_| VerificationError::InsufficientEnergy(energy_cost))?;
    
    // Update energy resource in state
    state.update_energy_resource(&self.source, energy_resource).await
        .map_err(VerificationError::State)?;
    
    debug!("Consumed {} energy for transaction {}", energy_cost, tx_hash);
}
```

### Complete Location in Apply Function

```rust
async fn apply<'a, P: ContractProvider, E, B: BlockchainApplyState<'a, P, E>>(
    &'a self,
    tx_hash: &'a Hash,
    state: &mut B,
    decompressed_deposits: &HashMap<&Hash, DecompressedDepositCt>,
) -> Result<(), VerificationError<E>> {
    trace!("Applying transaction data");
    
    // Update nonce
    state.update_account_nonce(self.get_source(), self.nonce + 1).await
        .map_err(VerificationError::State)?;

    // 🔥 ENERGY deduction logic - add here 🔥
    if self.uses_energy_for_fees() {
        let energy_cost = self.calculate_energy_cost();
        
        // Get user's energy resource
        let energy_resource = state.get_energy_resource(&self.source).await
            .map_err(VerificationError::State)?;
        
        // Check if user has enough energy
        if !energy_resource.has_enough_energy(energy_cost) {
            return Err(VerificationError::InsufficientEnergy(energy_cost));
        }
        
        // Consume energy
        let mut energy_resource = energy_resource;
        energy_resource.consume_energy(energy_cost)
            .map_err(|_| VerificationError::InsufficientEnergy(energy_cost))?;
        
        // Update energy resource in state
        state.update_energy_resource(&self.source, energy_resource).await
            .map_err(VerificationError::State)?;
        
        debug!("Consumed {} energy for transaction {}", energy_cost, tx_hash);
    }

    // Apply receiver balances
    match &self.data {
        TransactionType::Transfers(transfers) => {
            // ... existing code ...
        },
        // ... other transaction types ...
    }

    Ok(())
}
```

## Why Choose This Location?

### 1. Appropriate Timing
- **After nonce update**: Ensures correct transaction ordering
- **Before balance application**: Avoids state inconsistency

### 2. Clear Logic
- Energy deduction is the **first step** of transaction processing
- If energy is insufficient, immediately return error without proceeding

### 3. Error Handling
- Check energy before main business logic
- Ensure sufficient resources before transaction processing

## Required Prerequisites

### 1. Error Type Definition
Add in `common/src/transaction/verify/error.rs`:
```rust
#[error("Insufficient energy: required {0} energy")]
InsufficientEnergy(u64),
```

### 2. State Interface Extension
Add in `common/src/transaction/verify/state.rs` in the `BlockchainApplyState` trait:
```rust
/// Get energy resource for an account
async fn get_energy_resource(&self, account: &PublicKey) -> Result<EnergyResource, E>;

/// Update energy resource for an account
async fn update_energy_resource(&mut self, account: &PublicKey, energy: EnergyResource) -> Result<(), E>;
```

### 3. Transaction Methods
Add in `Transaction` impl:
```rust
/// Calculate energy cost for this transaction
pub fn calculate_energy_cost(&self) -> u64 { /* ... */ }

/// Check if this transaction uses energy for fees
pub fn uses_energy_for_fees(&self) -> bool { /* ... */ }
```

## Execution Flow

1. **Check fee mode**: `self.uses_energy_for_fees()`
2. **Calculate energy cost**: `self.calculate_energy_cost()`
3. **Get user energy resource**: `state.get_energy_resource()`
4. **Check if energy is sufficient**: `energy_resource.has_enough_energy()`
5. **Deduct energy**: `energy_resource.consume_energy()`
6. **Update state**: `state.update_energy_resource()`
7. **Log record**: `debug!` output consumed energy

## Error Handling

- **Insufficient energy**: Return `VerificationError::InsufficientEnergy`
- **State error**: Return `VerificationError::State`
- **Other errors**: Handle according to existing logic

This implementation ensures that energy deduction logic executes at the correct location and integrates perfectly with the existing transaction processing flow. 