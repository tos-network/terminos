use criterion::{black_box, criterion_group, criterion_main, Criterion};
use terminos_common::{
    account::energy::{EnergyResource, FreezeDuration},
    crypto::{KeyPair, Hash, elgamal::Ciphertext},
    transaction::{
        builder::{EnergyBuilder, TransactionBuilder, TransactionTypeBuilder, FeeBuilder, AccountState, FeeHelper},
        TxVersion, Reference,
    },
    account::{Nonce, CiphertextCache},
    config::COIN_VALUE,
};

/// Benchmark unfreeze_tos transaction building and validation
fn bench_unfreeze_tos_transaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("unfreeze_tos_transaction");
    
    // Test different unfreeze amounts
    let test_amounts = vec![
        100 * COIN_VALUE,   // 100 TOS
        500 * COIN_VALUE,   // 500 TOS
        1000 * COIN_VALUE,  // 1000 TOS
    ];
    
    for amount in test_amounts {
        group.bench_function(&format!("unfreeze_{}_tos", amount / COIN_VALUE), |b| {
            b.iter(|| {
                // Create test keypair
                let alice = KeyPair::new();
                
                // Create energy transaction builder for unfreeze
                let energy_builder = EnergyBuilder::unfreeze_tos(black_box(amount));
                let tx_type = TransactionTypeBuilder::Energy(energy_builder);
                let fee_builder = FeeBuilder::Value(20000); // 20000 TOS fee
                
                let builder = TransactionBuilder::new(
                    TxVersion::V0,
                    alice.get_public_key().compress(),
                    None,
                    tx_type,
                    fee_builder
                );
                
                // Create mock state
                let mut state = MockAccountState::new();
                state.set_balance(terminos_common::config::TERMINOS_ASSET, 2000 * COIN_VALUE);
                state.nonce = 0;
                
                // Build the transaction
                let _unfreeze_tx = builder.build(&mut state, &alice).unwrap();
            });
        });
    }
    
    group.finish();
}

/// Benchmark unfreeze_tos energy resource operations
fn bench_unfreeze_tos_energy_resource(c: &mut Criterion) {
    let mut group = c.benchmark_group("unfreeze_tos_energy_resource");
    
    // Test different scenarios
    let test_scenarios = vec![
        (100 * COIN_VALUE, FreezeDuration::Day3),
        (500 * COIN_VALUE, FreezeDuration::Day7),
        (1000 * COIN_VALUE, FreezeDuration::Day14),
    ];
    
    for (amount, duration) in test_scenarios {
        group.bench_function(&format!("unfreeze_{}_tos_{}_days", amount / COIN_VALUE, duration.duration_in_blocks() / (24 * 60 * 60)), |b| {
            b.iter(|| {
                // Create energy resource with frozen TOS
                let mut energy_resource = EnergyResource::new();
                let topoheight = 1000;
                
                // Freeze TOS first
                energy_resource.freeze_tos_for_energy(black_box(amount), duration.clone(), topoheight);
                
                // Simulate time passing (unlock time reached)
                let unlock_topoheight = topoheight + duration.duration_in_blocks();
                
                // Unfreeze TOS
                let unfreeze_amount = amount / 2; // Unfreeze half
                let _energy_removed = energy_resource.unfreeze_tos(unfreeze_amount, unlock_topoheight).unwrap();
            });
        });
    }
    
    group.finish();
}

/// Benchmark unfreeze_tos with multiple freeze records
fn bench_unfreeze_tos_multiple_records(c: &mut Criterion) {
    let mut group = c.benchmark_group("unfreeze_tos_multiple_records");
    
    group.bench_function("unfreeze_from_multiple_records", |b| {
        b.iter(|| {
            // Create energy resource with multiple freeze records
            let mut energy_resource = EnergyResource::new();
            let topoheight = 1000;
            
            // Create multiple freeze records with different durations
            let freeze_amounts = vec![
                100 * COIN_VALUE,
                200 * COIN_VALUE,
                300 * COIN_VALUE,
            ];
            let durations = vec![
                FreezeDuration::Day3,
                FreezeDuration::Day7,
                FreezeDuration::Day14,
            ];
            
            // Freeze TOS with different durations
            for (amount, duration) in freeze_amounts.iter().zip(durations.iter()) {
                energy_resource.freeze_tos_for_energy(*amount, duration.clone(), topoheight);
            }
            
            // Simulate time passing (all records unlocked)
            let max_duration = durations.iter().map(|d| d.duration_in_blocks()).max().unwrap();
            let unlock_topoheight = topoheight + max_duration;
            
            // Unfreeze from multiple records
            let unfreeze_amount = black_box(250 * COIN_VALUE);
            let _energy_removed = energy_resource.unfreeze_tos(unfreeze_amount, unlock_topoheight).unwrap();
        });
    });
    
    group.finish();
}

// Mock account state for benchmarking
struct MockAccountState {
    balances: std::collections::HashMap<Hash, u64>,
    nonce: u64,
    ciphertexts: std::collections::HashMap<Hash, CiphertextCache>,
}

impl MockAccountState {
    fn new() -> Self {
        Self {
            balances: std::collections::HashMap::new(),
            nonce: 0,
            ciphertexts: std::collections::HashMap::new(),
        }
    }
    
    fn set_balance(&mut self, asset: Hash, amount: u64) {
        self.balances.insert(asset, amount);
    }
}

impl FeeHelper for MockAccountState {
    type Error = std::io::Error;

    fn get_fee_multiplier(&self) -> f64 {
        1.0
    }

    fn account_exists(&self, _account: &terminos_common::crypto::elgamal::CompressedPublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

impl AccountState for MockAccountState {
    fn is_mainnet(&self) -> bool {
        false
    }
    
    fn get_account_balance(&self, asset: &Hash) -> Result<u64, Self::Error> {
        Ok(*self.balances.get(asset).unwrap_or(&0))
    }
    
    fn get_reference(&self) -> Reference {
        Reference {
            hash: Hash::zero(),
            topoheight: 1000,
        }
    }
    
    fn get_account_ciphertext(&self, asset: &Hash) -> Result<CiphertextCache, Self::Error> {
        Ok(self.ciphertexts.get(asset).cloned().unwrap_or_else(|| {
            // Create a default ciphertext cache
            CiphertextCache::Decompressed(Ciphertext::zero())
        }))
    }
    
    fn update_account_balance(&mut self, asset: &Hash, new_balance: u64, ciphertext: Ciphertext) -> Result<(), Self::Error> {
        self.balances.insert(asset.clone(), new_balance);
        self.ciphertexts.insert(asset.clone(), CiphertextCache::Decompressed(ciphertext));
        Ok(())
    }
    
    fn get_nonce(&self) -> Result<Nonce, Self::Error> {
        Ok(self.nonce)
    }
    
    fn update_nonce(&mut self, new_nonce: Nonce) -> Result<(), Self::Error> {
        self.nonce = new_nonce;
        Ok(())
    }
}

criterion_group!(
    energy_benches,
    bench_unfreeze_tos_transaction,
    bench_unfreeze_tos_energy_resource,
    bench_unfreeze_tos_multiple_records,
);
criterion_main!(energy_benches); 