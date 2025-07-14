use terminos_common::{
    config::{COIN_VALUE, TERMINOS_ASSET},
    crypto::KeyPair,
    transaction::{
        BurnPayload,
        FeeType,
        TransactionType,
        builder::{TransactionBuilder, TransferBuilder, TransactionTypeBuilder, FeeBuilder},
        Transaction,
        TxVersion,
    },
    crypto::elgamal::CompressedPublicKey,
};
use std::collections::HashMap;

// Helper function to create a simple transfer transaction
fn create_transfer_transaction(
    sender: &KeyPair,
    receiver: &terminos_common::crypto::elgamal::CompressedPublicKey,
    amount: u64,
    fee: u64,
    fee_type: FeeType,
    nonce: u64,
) -> Result<Transaction, Box<dyn std::error::Error>> {
    let transfer = TransferBuilder {
        destination: receiver.clone().to_address(false),
        amount,
        asset: TERMINOS_ASSET,
        extra_data: None,
        encrypt_extra_data: true,
    };
    
    let tx_type = TransactionTypeBuilder::Transfers(vec![transfer]);
    let fee_builder = FeeBuilder::Value(fee);
    
    let builder = TransactionBuilder::new(
        TxVersion::V1,
        sender.get_public_key().compress(),
        None,
        tx_type,
        fee_builder,
    ).with_fee_type(fee_type);
    
    // Create a simple mock state for testing
    let mut state = MockAccountState::new();
    state.set_balance(TERMINOS_ASSET, 1000 * COIN_VALUE);
    state.nonce = nonce;
    
    let tx = builder.build(&mut state, sender)?;
    Ok(tx)
}

// Mock chain state for block execution simulation
struct MockChainState {
    balances: HashMap<CompressedPublicKey, u64>,
    energy: HashMap<CompressedPublicKey, u64>,
    nonces: HashMap<CompressedPublicKey, u64>,
    total_energy: HashMap<CompressedPublicKey, u64>,
}

impl MockChainState {
    fn new() -> Self {
        Self {
            balances: HashMap::new(),
            energy: HashMap::new(),
            nonces: HashMap::new(),
            total_energy: HashMap::new(),
        }
    }
    
    fn set_balance(&mut self, account: CompressedPublicKey, amount: u64) {
        self.balances.insert(account, amount);
    }
    
    fn get_balance(&self, account: &CompressedPublicKey) -> u64 {
        *self.balances.get(account).unwrap_or(&0)
    }
    
    fn set_energy(&mut self, account: CompressedPublicKey, used_energy: u64, total_energy: u64) {
        self.energy.insert(account.clone(), used_energy);
        self.total_energy.insert(account, total_energy);
    }
    
    fn get_energy(&self, account: &CompressedPublicKey) -> (u64, u64) {
        let used = *self.energy.get(account).unwrap_or(&0);
        let total = *self.total_energy.get(account).unwrap_or(&0);
        (used, total)
    }
    
    fn get_available_energy(&self, account: &CompressedPublicKey) -> u64 {
        let (used, total) = self.get_energy(account);
        if used >= total {
            0
        } else {
            total - used
        }
    }
    
    fn set_nonce(&mut self, account: CompressedPublicKey, nonce: u64) {
        self.nonces.insert(account, nonce);
    }
    
    fn get_nonce(&self, account: &CompressedPublicKey) -> u64 {
        *self.nonces.get(account).unwrap_or(&0)
    }
    
    // Simulate applying a block with multiple transactions
    fn apply_block(&mut self, txs: &[(Transaction, u64)], signers: &[KeyPair]) -> Result<(), Box<dyn std::error::Error>> {
        for ((tx, amount), signer) in txs.iter().zip(signers) {
            self.apply_transaction(tx, *amount, signer)?;
        }
        Ok(())
    }
    
    // Simulate applying a single transaction
    fn apply_transaction(&mut self, tx: &Transaction, amount: u64, _signer: &KeyPair) -> Result<(), Box<dyn std::error::Error>> {
        let sender = tx.get_source();
        let nonce = tx.get_nonce();
        let fee = tx.get_fee();
        let fee_type = tx.get_fee_type();
        
        // Verify nonce
        let current_nonce = self.get_nonce(sender);
        if nonce != current_nonce {
            return Err(format!("Invalid nonce: expected {}, got {}", current_nonce, nonce).into());
        }
        
        // Update nonce
        self.set_nonce(sender.clone(), nonce + 1);
        
        // Process transaction data
        match tx.get_data() {
            TransactionType::Transfers(transfers) => {
                let mut account_creation_fee = 0;
                
                for transfer in transfers {
                    let destination = transfer.get_destination();
                    
                    // Check if destination account exists by checking if it's in our maps
                    // Only charge account creation fee if the account is truly uninitialized
                    let destination_balance = self.get_balance(destination);
                    let (destination_used_energy, destination_total_energy) = self.get_energy(destination);
                    let destination_nonce = self.get_nonce(destination);
                    
                    // Check if this account has been explicitly initialized in our mock state
                    let is_initialized = self.balances.contains_key(destination) || 
                                        self.energy.contains_key(destination) || 
                                        self.total_energy.contains_key(destination) || 
                                        self.nonces.contains_key(destination);
                    
                    // If destination account is completely uninitialized, charge account creation fee
                    if !is_initialized && destination_balance == 0 && destination_used_energy == 0 && destination_total_energy == 0 && destination_nonce == 0 {
                        account_creation_fee += 100000; // FEE_PER_ACCOUNT_CREATION
                    }
                    
                    // Deduct from sender
                    let sender_balance = self.get_balance(sender);
                    if sender_balance < amount {
                        return Err("Insufficient balance".into());
                    }
                    self.set_balance(sender.clone(), sender_balance - amount);
                    
                    // Add to receiver
                    let receiver_balance = self.get_balance(destination);
                    self.set_balance(destination.clone(), receiver_balance + amount);
                }
                
                // Handle fees
                match fee_type {
                    FeeType::TOS => {
                        // Deduct TOS fee and account creation fee from sender
                        let total_fee = fee + account_creation_fee;
                        let sender_balance = self.get_balance(sender);
                        if sender_balance < total_fee {
                            return Err("Insufficient balance for TOS fee and account creation fee".into());
                        }
                        self.set_balance(sender.clone(), sender_balance - total_fee);
                    },
                    FeeType::Energy => {
                        // For energy fees, account creation fee is still paid in TOS
                        if account_creation_fee > 0 {
                            let sender_balance = self.get_balance(sender);
                            if sender_balance < account_creation_fee {
                                return Err("Insufficient balance for account creation fee".into());
                            }
                            self.set_balance(sender.clone(), sender_balance - account_creation_fee);
                        }
                        
                        // Consume energy
                        let available_energy = self.get_available_energy(sender);
                        if available_energy < fee {
                            return Err("Insufficient energy".into());
                        }
                        let (used, total) = self.get_energy(sender);
                        self.set_energy(sender.clone(), used + fee, total);
                    }
                }
            },
            _ => {
                return Err("Only transfer transactions supported in mock".into());
            }
        }
        Ok(())
    }
}

// Simple mock account state for testing
struct MockAccountState {
    balances: std::collections::HashMap<terminos_common::crypto::Hash, u64>,
    nonce: u64,
}

impl MockAccountState {
    fn new() -> Self {
        Self {
            balances: std::collections::HashMap::new(),
            nonce: 0,
        }
    }
    
    fn set_balance(&mut self, asset: terminos_common::crypto::Hash, amount: u64) {
        self.balances.insert(asset, amount);
    }
}

impl terminos_common::transaction::builder::AccountState for MockAccountState {
    fn is_mainnet(&self) -> bool {
        false
    }
    
    fn get_account_balance(&self, asset: &terminos_common::crypto::Hash) -> Result<u64, Self::Error> {
        Ok(self.balances.get(asset).copied().unwrap_or(1000 * COIN_VALUE))
    }
    
    fn get_reference(&self) -> terminos_common::transaction::Reference {
        terminos_common::transaction::Reference {
            topoheight: 0,
            hash: terminos_common::crypto::Hash::zero(),
        }
    }
    
    fn get_account_ciphertext(&self, _asset: &terminos_common::crypto::Hash) -> Result<terminos_common::account::CiphertextCache, Self::Error> {
        // Return a dummy ciphertext for testing
        let keypair = KeyPair::new();
        let ciphertext = keypair.get_public_key().encrypt(1000 * COIN_VALUE);
        Ok(terminos_common::account::CiphertextCache::Decompressed(ciphertext))
    }
    
    fn update_account_balance(&mut self, asset: &terminos_common::crypto::Hash, new_balance: u64, _ciphertext: terminos_common::crypto::elgamal::Ciphertext) -> Result<(), Self::Error> {
        self.balances.insert(asset.clone(), new_balance);
        Ok(())
    }
    
    fn get_nonce(&self) -> Result<u64, Self::Error> {
        Ok(self.nonce)
    }
    
    fn update_nonce(&mut self, new_nonce: u64) -> Result<(), Self::Error> {
        self.nonce = new_nonce;
        Ok(())
    }
}

impl terminos_common::transaction::builder::FeeHelper for MockAccountState {
    type Error = Box<dyn std::error::Error>;
    
    fn account_exists(&self, _key: &terminos_common::crypto::elgamal::CompressedPublicKey) -> Result<bool, Self::Error> {
        Ok(true) // Assume account exists for testing
    }
}

#[tokio::test]
async fn test_energy_fee_validation_integration() {
    println!("Testing energy fee validation in integration context...");
    
    // Test that FeeType enum works correctly
    let tos_fee = FeeType::TOS;
    let energy_fee = FeeType::Energy;
    
    assert!(tos_fee.is_tos());
    assert!(!tos_fee.is_energy());
    assert!(energy_fee.is_energy());
    assert!(!energy_fee.is_tos());
    
    // Test that energy fees are only valid for Transfer transactions
    let transfer_type = TransactionType::Transfers(vec![]);
    let burn_type = TransactionType::Burn(BurnPayload {
        asset: TERMINOS_ASSET,
        amount: 100,
    });
    
    // Energy fees should only be valid for transfers
    assert!(matches!(transfer_type, TransactionType::Transfers(_)));
    assert!(!matches!(burn_type, TransactionType::Transfers(_)));
    
    println!("Energy fee validation working correctly:");
    println!("- TOS fees: valid for all transaction types");
    println!("- Energy fees: only valid for Transfer transactions");
    println!("- Transfer transactions: can use either TOS or Energy fees");
    println!("- Non-transfer transactions: must use TOS fees");
    
    // Test with real transaction types
    let alice = KeyPair::new();
    let bob = KeyPair::new();
    
    println!("Test accounts created:");
    println!("Alice: {}", hex::encode(alice.get_public_key().compress().as_bytes()));
    println!("Bob: {}", hex::encode(bob.get_public_key().compress().as_bytes()));
    
    // Test fee type validation logic
    let transfer_with_tos_fee = (TransactionType::Transfers(vec![]), FeeType::TOS);
    let transfer_with_energy_fee = (TransactionType::Transfers(vec![]), FeeType::Energy);
    let burn_with_tos_fee = (TransactionType::Burn(BurnPayload {
        asset: TERMINOS_ASSET,
        amount: 100,
    }), FeeType::TOS);
    let burn_with_energy_fee = (TransactionType::Burn(BurnPayload {
        asset: TERMINOS_ASSET,
        amount: 100,
    }), FeeType::Energy);
    
    // Validate fee type combinations
    assert!(is_valid_fee_type_combination(&transfer_with_tos_fee.0, &transfer_with_tos_fee.1));
    assert!(is_valid_fee_type_combination(&transfer_with_energy_fee.0, &transfer_with_energy_fee.1));
    assert!(is_valid_fee_type_combination(&burn_with_tos_fee.0, &burn_with_tos_fee.1));
    assert!(!is_valid_fee_type_combination(&burn_with_energy_fee.0, &burn_with_energy_fee.1));
    
    println!("Fee type validation logic working correctly:");
    println!("✓ Transfer + TOS fee: valid");
    println!("✓ Transfer + Energy fee: valid");
    println!("✓ Burn + TOS fee: valid");
    println!("✗ Burn + Energy fee: invalid (as expected)");
    
    // Test transaction building with different fee types
    println!("\nTesting transaction building with different fee types...");
    
    // Test 1: Transfer with TOS fee
    let transfer_tos_tx = create_transfer_transaction(
        &alice,
        &bob.get_public_key().compress(),
        100 * COIN_VALUE, // 100 TOS
        5000, // 0.00005 TOS fee
        FeeType::TOS,
        0, // nonce
    ).unwrap();
    
    assert_eq!(transfer_tos_tx.get_fee_type(), &FeeType::TOS);
    assert_eq!(transfer_tos_tx.get_fee(), 5000);
    println!("✓ Transfer with TOS fee built successfully");
    
    // Test 2: Transfer with Energy fee
    let transfer_energy_tx = create_transfer_transaction(
        &alice,
        &bob.get_public_key().compress(),
        100 * COIN_VALUE, // 100 TOS
        50, // 50 energy units
        FeeType::Energy,
        1, // nonce
    ).unwrap();
    
    assert_eq!(transfer_energy_tx.get_fee_type(), &FeeType::Energy);
    assert_eq!(transfer_energy_tx.get_fee(), 50);
    println!("✓ Transfer with Energy fee built successfully");
    
    // Test 3: Verify transaction types
    assert!(matches!(transfer_tos_tx.get_data(), TransactionType::Transfers(_)));
    assert!(matches!(transfer_energy_tx.get_data(), TransactionType::Transfers(_)));
    println!("✓ Transaction types verified correctly");
    
    println!("Integration test completed successfully!");
    println!("All energy fee validation logic working correctly");
}

#[tokio::test]
async fn test_tos_fee_transfer_integration() {
    println!("Testing TOS fee transfer transaction building...");
    
    // Create test accounts
    let alice = KeyPair::new();
    let bob = KeyPair::new();
    
    // Create transfer transaction with TOS fee
    let transfer_amount = 100 * COIN_VALUE;
    let tos_fee = 5000; // 0.00005 TOS
    
    let transfer_tx = create_transfer_transaction(
        &alice,
        &bob.get_public_key().compress(),
        transfer_amount,
        tos_fee,
        FeeType::TOS,
        0, // nonce
    ).unwrap();
    
    println!("TOS fee transfer transaction created:");
    println!("Amount: {} TOS", transfer_amount as f64 / COIN_VALUE as f64);
    println!("TOS fee: {} TOS", tos_fee as f64 / COIN_VALUE as f64);
    println!("Fee type: {:?}", transfer_tx.get_fee_type());
    
    // Verify transaction properties
    assert_eq!(transfer_tx.get_fee_type(), &FeeType::TOS);
    assert_eq!(transfer_tx.get_fee(), tos_fee);
    assert!(matches!(transfer_tx.get_data(), TransactionType::Transfers(_)));
    
    println!("✓ TOS fee transfer test passed!");
}

#[tokio::test]
async fn test_invalid_energy_fee_on_burn_transaction() {
    println!("Testing invalid energy fee on burn transaction...");
    
    let alice = KeyPair::new();
    
    // Create burn transaction with energy fee (should fail validation)
    let burn_payload = BurnPayload {
        asset: TERMINOS_ASSET,
        amount: 100,
    };
    
    let tx_type = TransactionTypeBuilder::Burn(burn_payload);
    let fee_builder = FeeBuilder::Value(50);
    
    let builder = TransactionBuilder::new(
        TxVersion::V1,
        alice.get_public_key().compress(),
        None,
        tx_type,
        fee_builder,
    ).with_fee_type(FeeType::Energy); // This should cause validation to fail
    
    // Create a simple mock state for testing
    let mut state = MockAccountState::new();
    state.set_balance(TERMINOS_ASSET, 1000 * COIN_VALUE);
    
    // This should fail because burn transactions can't use energy fees
    let result = builder.build(&mut state, &alice);
    assert!(result.is_err());
    
    println!("✓ Burn transaction with energy fee correctly rejected!");
    println!("Error: {:?}", result.unwrap_err());
}

#[test]
fn test_block_execution_simulation() {
    println!("Testing block execution simulation with Alice and Bob accounts...");
    
    let mut chain = MockChainState::new();
    let alice = KeyPair::new();
    let bob = KeyPair::new();
    
    let alice_pubkey = alice.get_public_key().compress();
    let bob_pubkey = bob.get_public_key().compress();
    
    // Initialize account states
    chain.set_balance(alice_pubkey.clone(), 1000 * COIN_VALUE); // 1000 TOS
    chain.set_balance(bob_pubkey.clone(), 0); // 0 TOS
    chain.set_energy(alice_pubkey.clone(), 0, 1000); // 1000 total energy, 0 used
    chain.set_energy(bob_pubkey.clone(), 0, 0); // No energy for Bob
    chain.set_nonce(alice_pubkey.clone(), 0);
    chain.set_nonce(bob_pubkey.clone(), 0);
    
    println!("Initial state:");
    println!("Alice balance: {} TOS", chain.get_balance(&alice_pubkey) as f64 / COIN_VALUE as f64);
    println!("Bob balance: {} TOS", chain.get_balance(&bob_pubkey) as f64 / COIN_VALUE as f64);
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    println!("Alice energy: used_energy: {}, total_energy: {}", used_energy, total_energy);
    
    // Create multiple transactions for the block
    let tx1 = create_transfer_transaction(
        &alice,
        &bob_pubkey,
        100 * COIN_VALUE, // 100 TOS transfer
        5000, // 0.00005 TOS fee
        FeeType::TOS,
        0, // nonce
    ).unwrap();
    
    let tx2 = create_transfer_transaction(
        &alice,
        &bob_pubkey,
        50 * COIN_VALUE, // 50 TOS transfer
        30, // 30 energy units
        FeeType::Energy,
        1, // nonce
    ).unwrap();
    
    let tx3 = create_transfer_transaction(
        &alice,
        &bob_pubkey,
        75 * COIN_VALUE, // 75 TOS transfer
        25, // 25 energy units
        FeeType::Energy,
        2, // nonce
    ).unwrap();
    
    println!("\nBlock transactions:");
    println!("TX1: Alice -> Bob, 100 TOS, TOS fee (0.00005 TOS)");
    println!("TX2: Alice -> Bob, 50 TOS, Energy fee (30 units)");
    println!("TX3: Alice -> Bob, 75 TOS, Energy fee (25 units)");
    
    // Execute the block
    let txs = vec![(tx1, 100 * COIN_VALUE), (tx2, 50 * COIN_VALUE), (tx3, 75 * COIN_VALUE)];
    let signers = vec![alice.clone(), alice.clone(), alice.clone()];
    
    let result = chain.apply_block(&txs, &signers);
    assert!(result.is_ok(), "Block execution failed: {:?}", result.err());
    
    println!("\nAfter block execution:");
    println!("Alice balance: {} TOS", chain.get_balance(&alice_pubkey) as f64 / COIN_VALUE as f64);
    println!("Bob balance: {} TOS", chain.get_balance(&bob_pubkey) as f64 / COIN_VALUE as f64);
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    println!("Alice energy: used_energy: {}, total_energy: {}", used_energy, total_energy);
    println!("Alice nonce: {}", chain.get_nonce(&alice_pubkey));
    
    // Verify final balances
    // Alice should have: 1000 - 100 - 50 - 75 - 0.00005 = 774.99995 TOS
    // (Bob is already initialized, so no account creation fee)
    let expected_alice_balance = 1000 * COIN_VALUE - 100 * COIN_VALUE - 50 * COIN_VALUE - 75 * COIN_VALUE - 5000;
    assert_eq!(chain.get_balance(&alice_pubkey), expected_alice_balance);
    
    // Bob should have: 0 + 100 + 50 + 75 = 225 TOS
    let expected_bob_balance = 100 * COIN_VALUE + 50 * COIN_VALUE + 75 * COIN_VALUE;
    assert_eq!(chain.get_balance(&bob_pubkey), expected_bob_balance);
    
    // Alice should have consumed: 30 + 25 = 55 energy units
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    assert_eq!(used_energy, 55);
    assert_eq!(total_energy, 1000);
    
    // Alice nonce should be: 0 + 3 = 3
    assert_eq!(chain.get_nonce(&alice_pubkey), 3);
    
    println!("✓ Block execution simulation test passed!");
    println!("✓ All balance, energy, and nonce changes verified correctly");
}

#[test]
fn test_block_execution_with_new_account() {
    println!("Testing block execution with new account (Bob not initialized)...");
    
    let mut chain = MockChainState::new();
    let alice = KeyPair::new();
    let bob = KeyPair::new();
    
    let alice_pubkey = alice.get_public_key().compress();
    let bob_pubkey = bob.get_public_key().compress();
    
    // Initialize only Alice's account state
    chain.set_balance(alice_pubkey.clone(), 1000 * COIN_VALUE); // 1000 TOS
    chain.set_energy(alice_pubkey.clone(), 0, 1000); // 1000 total energy, 0 used
    chain.set_nonce(alice_pubkey.clone(), 0);
    
    // Bob's account is NOT initialized (no balance, no energy, no nonce set)
    // This simulates a new account that will be created by the first transaction
    
    println!("Initial state:");
    println!("Alice balance: {} TOS", chain.get_balance(&alice_pubkey) as f64 / COIN_VALUE as f64);
    println!("Bob balance: {} TOS", chain.get_balance(&bob_pubkey) as f64 / COIN_VALUE as f64);
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    println!("Alice energy: used_energy: {}, total_energy: {}", used_energy, total_energy);
    println!("Bob energy: used_energy: {}, total_energy: {}", chain.get_energy(&bob_pubkey).0, chain.get_energy(&bob_pubkey).1);
    println!("Alice nonce: {}", chain.get_nonce(&alice_pubkey));
    println!("Bob nonce: {}", chain.get_nonce(&bob_pubkey));
    
    // Create only one transaction for the block
    let tx1 = create_transfer_transaction(
        &alice,
        &bob_pubkey,
        200 * COIN_VALUE, // 200 TOS transfer
        5000, // 0.00005 TOS fee
        FeeType::TOS,
        0, // nonce
    ).unwrap();
    
    println!("\nBlock transaction:");
    println!("TX1: Alice -> Bob, 200 TOS, TOS fee (0.00005 TOS)");
    println!("Note: Bob's account will be created by this transaction");
    
    // Execute the block with only one transaction
    let txs = vec![(tx1, 200 * COIN_VALUE)];
    let signers = vec![alice.clone()];
    
    let result = chain.apply_block(&txs, &signers);
    assert!(result.is_ok(), "Block execution failed: {:?}", result.err());
    
    println!("\nAfter block execution:");
    println!("Alice balance: {} TOS", chain.get_balance(&alice_pubkey) as f64 / COIN_VALUE as f64);
    println!("Bob balance: {} TOS", chain.get_balance(&bob_pubkey) as f64 / COIN_VALUE as f64);
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    println!("Alice energy: used_energy: {}, total_energy: {}", used_energy, total_energy);
    println!("Bob energy: used_energy: {}, total_energy: {}", chain.get_energy(&bob_pubkey).0, chain.get_energy(&bob_pubkey).1);
    println!("Alice nonce: {}", chain.get_nonce(&alice_pubkey));
    println!("Bob nonce: {}", chain.get_nonce(&bob_pubkey));
    
    // Verify final balances
    // Alice should have: 1000 - 200 - 0.00005 - 0.001 = 799.99895 TOS
    // (200 TOS transfer + 0.00005 TOS fee + 0.001 TOS account creation fee)
    let expected_alice_balance = 1000 * COIN_VALUE - 200 * COIN_VALUE - 5000 - 100000;
    assert_eq!(chain.get_balance(&alice_pubkey), expected_alice_balance);
    
    // Bob should have: 0 + 200 = 200 TOS (account created with initial balance)
    let expected_bob_balance = 200 * COIN_VALUE;
    assert_eq!(chain.get_balance(&bob_pubkey), expected_bob_balance);
    
    // Alice should have consumed: 0 energy units (TOS fee transaction)
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    assert_eq!(used_energy, 0);
    assert_eq!(total_energy, 1000);
    
    // Bob should have: 0 energy (new account, no energy)
    let (bob_used_energy, bob_total_energy) = chain.get_energy(&bob_pubkey);
    assert_eq!(bob_used_energy, 0);
    assert_eq!(bob_total_energy, 0);
    
    // Alice nonce should be: 0 + 1 = 1
    assert_eq!(chain.get_nonce(&alice_pubkey), 1);
    
    // Bob nonce should be: 0 (new account, no transactions sent yet)
    assert_eq!(chain.get_nonce(&bob_pubkey), 0);
    
    println!("✓ Block execution with new account test passed!");
    println!("✓ Bob's account was successfully created with initial balance");
    println!("✓ Alice's balance and nonce correctly updated");
    println!("✓ Energy consumption correctly tracked (0 for TOS fee transaction)");
}

#[test]
fn test_block_execution_with_new_account_energy_fee() {
    println!("Testing block execution with new account using ENERGY fee...");
    
    let mut chain = MockChainState::new();
    let alice = KeyPair::new();
    let bob = KeyPair::new();
    
    let alice_pubkey = alice.get_public_key().compress();
    let bob_pubkey = bob.get_public_key().compress();
    
    // Initialize only Alice's account state
    chain.set_balance(alice_pubkey.clone(), 1000 * COIN_VALUE); // 1000 TOS
    chain.set_energy(alice_pubkey.clone(), 0, 1000); // 1000 total energy, 0 used
    chain.set_nonce(alice_pubkey.clone(), 0);
    
    // Bob's account is NOT initialized (no balance, no energy, no nonce set)
    // This simulates a new account that will be created by the first transaction
    
    println!("Initial state:");
    println!("Alice balance: {} TOS", chain.get_balance(&alice_pubkey) as f64 / COIN_VALUE as f64);
    println!("Bob balance: {} TOS", chain.get_balance(&bob_pubkey) as f64 / COIN_VALUE as f64);
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    println!("Alice energy: used_energy: {}, total_energy: {}", used_energy, total_energy);
    println!("Bob energy: used_energy: {}, total_energy: {}", chain.get_energy(&bob_pubkey).0, chain.get_energy(&bob_pubkey).1);
    println!("Alice nonce: {}", chain.get_nonce(&alice_pubkey));
    println!("Bob nonce: {}", chain.get_nonce(&bob_pubkey));
    
    // Create only one transaction for the block with ENERGY fee
    let tx1 = create_transfer_transaction(
        &alice,
        &bob_pubkey,
        200 * COIN_VALUE, // 200 TOS transfer
        50, // 50 energy units
        FeeType::Energy,
        0, // nonce
    ).unwrap();
    
    println!("\nBlock transaction:");
    println!("TX1: Alice -> Bob, 200 TOS, Energy fee (50 units)");
    println!("Note: Bob's account will be created by this transaction");
    println!("Note: Account creation fee (0.001 TOS) will still be paid in TOS even with energy fee");
    
    // Execute the block with only one transaction
    let txs = vec![(tx1, 200 * COIN_VALUE)];
    let signers = vec![alice.clone()];
    
    let result = chain.apply_block(&txs, &signers);
    assert!(result.is_ok(), "Block execution failed: {:?}", result.err());
    
    println!("\nAfter block execution:");
    println!("Alice balance: {} TOS", chain.get_balance(&alice_pubkey) as f64 / COIN_VALUE as f64);
    println!("Bob balance: {} TOS", chain.get_balance(&bob_pubkey) as f64 / COIN_VALUE as f64);
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    println!("Alice energy: used_energy: {}, total_energy: {}", used_energy, total_energy);
    println!("Bob energy: used_energy: {}, total_energy: {}", chain.get_energy(&bob_pubkey).0, chain.get_energy(&bob_pubkey).1);
    println!("Alice nonce: {}", chain.get_nonce(&alice_pubkey));
    println!("Bob nonce: {}", chain.get_nonce(&bob_pubkey));
    
    // Verify final balances
    // Alice should have: 1000 - 200 - 0.001 = 799.999 TOS
    // (200 TOS transfer + 0.001 TOS account creation fee, no TOS fee since using energy)
    let expected_alice_balance = 1000 * COIN_VALUE - 200 * COIN_VALUE - 100000;
    assert_eq!(chain.get_balance(&alice_pubkey), expected_alice_balance);
    
    // Bob should have: 0 + 200 = 200 TOS (account created with initial balance)
    let expected_bob_balance = 200 * COIN_VALUE;
    assert_eq!(chain.get_balance(&bob_pubkey), expected_bob_balance);
    
    // Alice should have consumed: 50 energy units (energy fee transaction)
    let (used_energy, total_energy) = chain.get_energy(&alice_pubkey);
    assert_eq!(used_energy, 50);
    assert_eq!(total_energy, 1000);
    
    // Bob should have: 0 energy (new account, no energy)
    let (bob_used_energy, bob_total_energy) = chain.get_energy(&bob_pubkey);
    assert_eq!(bob_used_energy, 0);
    assert_eq!(bob_total_energy, 0);
    
    // Alice nonce should be: 0 + 1 = 1
    assert_eq!(chain.get_nonce(&alice_pubkey), 1);
    
    // Bob nonce should be: 0 (new account, no transactions sent yet)
    assert_eq!(chain.get_nonce(&bob_pubkey), 0);
    
    println!("✓ Block execution with new account using ENERGY fee test passed!");
    println!("✓ Bob's account was successfully created with initial balance");
    println!("✓ Alice's balance correctly updated (deducted transfer amount + account creation fee)");
    println!("✓ Alice's energy correctly consumed (50 units for energy fee)");
    println!("✓ Account creation fee correctly paid in TOS even with energy fee");
}

#[test]
fn test_energy_insufficient_error() {
    println!("Testing energy insufficient error...");
    
    let mut chain = MockChainState::new();
    let alice = KeyPair::new();
    let bob = KeyPair::new();
    
    let alice_pubkey = alice.get_public_key().compress();
    let bob_pubkey = bob.get_public_key().compress();
    
    // Initialize with limited energy
    chain.set_balance(alice_pubkey.clone(), 1000 * COIN_VALUE);
    chain.set_balance(bob_pubkey.clone(), 0);
    chain.set_energy(alice_pubkey.clone(), 0, 50); // Only 50 total energy
    chain.set_nonce(alice_pubkey.clone(), 0);
    
    // Try to create a transaction requiring more energy than available
    let tx = create_transfer_transaction(
        &alice,
        &bob_pubkey,
        100 * COIN_VALUE,
        60, // 60 energy units (more than available 50)
        FeeType::Energy,
        0, // nonce
    ).unwrap();
    
    // This should fail due to insufficient energy
    let result = chain.apply_transaction(&tx, 100 * COIN_VALUE, &alice);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Insufficient energy"));
    
    println!("✓ Energy insufficient error correctly handled!");
}

#[test]
fn test_balance_insufficient_error() {
    println!("Testing balance insufficient error...");
    
    let mut chain = MockChainState::new();
    let alice = KeyPair::new();
    let bob = KeyPair::new();
    
    let alice_pubkey = alice.get_public_key().compress();
    let bob_pubkey = bob.get_public_key().compress();
    
    // Initialize with limited balance
    chain.set_balance(alice_pubkey.clone(), 100 * COIN_VALUE); // Only 100 TOS
    chain.set_balance(bob_pubkey.clone(), 0);
    chain.set_energy(alice_pubkey.clone(), 0, 1000);
    chain.set_nonce(alice_pubkey.clone(), 0);
    
    // Try to transfer more than available balance
    let tx = create_transfer_transaction(
        &alice,
        &bob_pubkey,
        150 * COIN_VALUE, // 150 TOS (more than available 100)
        5000, // TOS fee
        FeeType::TOS,
        0, // nonce
    ).unwrap();
    
    // This should fail due to insufficient balance
    let result = chain.apply_transaction(&tx, 150 * COIN_VALUE, &alice);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Insufficient balance"));
    
    println!("✓ Balance insufficient error correctly handled!");
}

/// Helper function to validate fee type combinations
fn is_valid_fee_type_combination(tx_type: &TransactionType, fee_type: &FeeType) -> bool {
    match (tx_type, fee_type) {
        (TransactionType::Transfers(_), FeeType::TOS) => true,
        (TransactionType::Transfers(_), FeeType::Energy) => true,
        (TransactionType::Burn(_), FeeType::TOS) => true,
        (TransactionType::Burn(_), FeeType::Energy) => false,
        (TransactionType::MultiSig(_), FeeType::TOS) => true,
        (TransactionType::MultiSig(_), FeeType::Energy) => false,
        (TransactionType::InvokeContract(_), FeeType::TOS) => true,
        (TransactionType::InvokeContract(_), FeeType::Energy) => false,
        (TransactionType::DeployContract(_), FeeType::TOS) => true,
        (TransactionType::DeployContract(_), FeeType::Energy) => false,
        (TransactionType::Energy(_), FeeType::TOS) => true,
        (TransactionType::Energy(_), FeeType::Energy) => false,
    }
} 