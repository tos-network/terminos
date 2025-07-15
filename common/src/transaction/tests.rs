use std::{borrow::Cow, collections::HashMap};
use async_trait::async_trait;
use curve25519_dalek::Scalar;
use indexmap::IndexSet;
use terminos_vm::{Chunk, Environment, Module};
use crate::{
    account::{CiphertextCache, Nonce},
    api::{DataElement, DataValue},
    block::BlockVersion,
    config::{BURN_PER_CONTRACT, COIN_VALUE, TERMINOS_ASSET},
    crypto::{
        elgamal::{Ciphertext, PedersenOpening},
        proofs::PC_GENS,
        Address,
        Hash,
        Hashable,
        KeyPair,
        PublicKey
    },
    serializer::Serializer,
    transaction::{
        builder::{ContractDepositBuilder, DeployContractBuilder, InvokeContractBuilder},
        MultiSigPayload,
        TransactionType,
        TxVersion,
        MAX_TRANSFER_COUNT
    }
};
use super::{
    extra_data::{
        derive_shared_key_from_opening,
        PlaintextData
    },
    builder::{
        AccountState,
        FeeBuilder,
        FeeHelper,
        TransactionBuilder,
        TransactionTypeBuilder,
        TransferBuilder,
        MultiSigBuilder,
        GenerationError
    },
    verify::BlockchainVerificationState,
    BurnPayload,
    FeeType,
    Reference,
    Role,
    Transaction
};

struct AccountChainState {
    balances: HashMap<Hash, Ciphertext>,
    nonce: Nonce,
}

struct ChainState {
    accounts: HashMap<PublicKey, AccountChainState>,
    multisig: HashMap<PublicKey, MultiSigPayload>,
    contracts: HashMap<Hash, Module>,
    env: Environment,
}

impl ChainState {
    fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            multisig: HashMap::new(),
            contracts: HashMap::new(),
            env: Environment::new(),
        }
    }
}

#[derive(Clone)]
struct Balance {
    ciphertext: CiphertextCache,
    balance: u64,
}

#[derive(Clone)]
struct Account {
    balances: HashMap<Hash, Balance>,
    keypair: KeyPair,
    nonce: Nonce,
}

impl Account {
    fn new() -> Self {
        Self {
            balances: HashMap::new(),
            keypair: KeyPair::new(),
            nonce: 0,
        }
    }

    fn set_balance(&mut self, asset: Hash, balance: u64) {
        let ciphertext = self.keypair.get_public_key().encrypt(balance);
        self.balances.insert(asset, Balance {
            balance,
            ciphertext: CiphertextCache::Decompressed(ciphertext),
        });
    }

    fn address(&self) -> Address {
        self.keypair.get_public_key().to_address(false)
    }
}

struct AccountStateImpl {
    balances: HashMap<Hash, Balance>,
    reference: Reference,
    nonce: Nonce,
}

fn create_tx_for(account: Account, destination: Address, amount: u64, extra_data: Option<DataElement>) -> Transaction {
    let mut state = AccountStateImpl {
        balances: account.balances,
        nonce: account.nonce,
        reference: Reference {
            topoheight: 0,
            hash: Hash::zero(),
        },
    };

    let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
        amount,
        destination,
        asset: TERMINOS_ASSET,
        extra_data,
        encrypt_extra_data: true,
    }]);


    let builder = TransactionBuilder::new(TxVersion::V1, account.keypair.get_public_key().compress(), None, data, FeeBuilder::default());
    let estimated_size = builder.estimate_size();
    let tx = builder.build(&mut state, &account.keypair).unwrap();
    assert!(estimated_size == tx.size(), "expected {} bytes got {} bytes", tx.size(), estimated_size);
    assert!(tx.to_bytes().len() == estimated_size);

    tx
}

#[test]
fn test_encrypt_decrypt() {
    let r = PedersenOpening::generate_new();
    let key = derive_shared_key_from_opening(&r);
    let message = "Hello, World!".as_bytes().to_vec();

    let plaintext = PlaintextData(message.clone());
    let cipher = plaintext.encrypt_in_place_with_aead(&key);
    let decrypted = cipher.decrypt_in_place(&key).unwrap();

    assert_eq!(decrypted.0, message);
}

#[test]
fn test_encrypt_decrypt_two_parties() {
    let mut alice = Account::new();
    alice.balances.insert(TERMINOS_ASSET, Balance {
        balance: 100 * COIN_VALUE,
        ciphertext: CiphertextCache::Decompressed(alice.keypair.get_public_key().encrypt(100 * COIN_VALUE)),
    });

    let bob = Account::new();

    let payload = DataElement::Value(DataValue::String("Hello, World!".to_string()));
    let tx = create_tx_for(alice.clone(), bob.address(), 50, Some(payload.clone()));
    let TransactionType::Transfers(transfers) = tx.get_data() else {
        unreachable!()
    };

    let transfer = &transfers[0];
    let cipher = transfer.get_extra_data().clone().unwrap();
    // Verify the extra data from alice (sender)
    {
        let decrypted = cipher.decrypt(&alice.keypair.get_private_key(), None, Role::Sender).unwrap();
        assert_eq!(decrypted.data(), Some(&payload));
    }

    // Verify the extra data from bob (receiver)
    {
        let decrypted = cipher.decrypt(&bob.keypair.get_private_key(), None, Role::Receiver).unwrap();
        assert_eq!(decrypted.data(), Some(&payload));
    }

    // Verify the extra data from alice (sender) with the wrong key
    {
        let decrypted = cipher.decrypt(&bob.keypair.get_private_key(), None, Role::Sender);
        assert!(decrypted.is_err());
    }
}


#[tokio::test]
async fn test_tx_verify() {
    let mut alice = Account::new();
    let mut bob = Account::new();

    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    bob.set_balance(TERMINOS_ASSET, 0);

    // Alice account is cloned to not be updated as it is used for verification and need current state
    let tx = create_tx_for(alice.clone(), bob.address(), 50, None);

    let mut state = ChainState::new();

    // Create the chain state
    {
        let mut balances = HashMap::new();
        for (asset, balance) in &alice.balances {
            balances.insert(asset.clone(), balance.ciphertext.clone().take_ciphertext().unwrap());
        }
        state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    {
        let mut balances = HashMap::new();
        for (asset, balance) in &bob.balances {
            balances.insert(asset.clone(), balance.ciphertext.clone().take_ciphertext().unwrap());
        }
        state.accounts.insert(bob.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    let hash = tx.hash();
    tx.verify(&hash, &mut state).await.unwrap();

    // Check Bob balance
    let balance = bob.keypair.decrypt_to_point(&state.accounts[&bob.keypair.get_public_key().compress()].balances[&TERMINOS_ASSET]);    
    assert_eq!(balance, Scalar::from(50u64) * PC_GENS.B);

    // Check Alice balance
    let balance = alice.keypair.decrypt_to_point(&state.accounts[&alice.keypair.get_public_key().compress()].balances[&TERMINOS_ASSET]);
    assert_eq!(balance, Scalar::from((100u64 * COIN_VALUE) - (50 + tx.fee)) * PC_GENS.B);
}

#[tokio::test]
async fn test_burn_tx_verify() {
    let mut alice = Account::new();
    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);

    let tx = {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
    
        let data = TransactionTypeBuilder::Burn(BurnPayload {
            amount: 50 * COIN_VALUE,
            asset: TERMINOS_ASSET,
        });
        let builder = TransactionBuilder::new(TxVersion::V0, alice.keypair.get_public_key().compress(), None, data, FeeBuilder::default());
        let estimated_size = builder.estimate_size();
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(estimated_size == tx.size());
        assert!(tx.to_bytes().len() == estimated_size);

        tx
    };

    let mut state = ChainState::new();

    // Create the chain state
    {
        let mut balances = HashMap::new();
        for (asset, balance) in &alice.balances {
            balances.insert(asset.clone(), balance.ciphertext.clone().take_ciphertext().unwrap());
        }
        state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    let hash = tx.hash();
    tx.verify(&hash, &mut state).await.unwrap();

    // Check Alice balance
    let balance = alice.keypair.decrypt_to_point(&state.accounts[&alice.keypair.get_public_key().compress()].balances[&TERMINOS_ASSET]);
    assert_eq!(balance, Scalar::from((100u64 * COIN_VALUE) - (50 * COIN_VALUE + tx.fee)) * PC_GENS.B);
}

#[tokio::test]
async fn test_tx_invoke_contract() {
    let mut alice = Account::new();

    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);

    let tx = {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };

        let data = TransactionTypeBuilder::InvokeContract(InvokeContractBuilder {
            contract: Hash::zero(),
            chunk_id: 0,
            max_gas: 1000,
            parameters: Vec::new(),
            deposits: [
                (TERMINOS_ASSET, ContractDepositBuilder {
                    amount: 50 * COIN_VALUE,
                    private: false
                })
            ].into_iter().collect()
        });
        let builder = TransactionBuilder::new(TxVersion::V2, alice.keypair.get_public_key().compress(), None, data, FeeBuilder::default());
        let estimated_size = builder.estimate_size();
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(estimated_size == tx.size());
        assert!(tx.to_bytes().len() == estimated_size);

        tx
    };

    let mut state = ChainState::new();
    let mut module = Module::new();
    module.add_entry_chunk(Chunk::new());
    state.contracts.insert(Hash::zero(), module);

    // Create the chain state
    {
        let mut balances = HashMap::new();
        for (asset, balance) in &alice.balances {
            balances.insert(asset.clone(), balance.ciphertext.clone().take_ciphertext().unwrap());
        }
        state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    let hash = tx.hash();
    tx.verify(&hash, &mut state).await.unwrap();

    // Check Alice balance
    let balance = alice.keypair.decrypt_to_point(&state.accounts[&alice.keypair.get_public_key().compress()].balances[&TERMINOS_ASSET]);
    // 50 coins deposit + tx fee + 1000 gas fee
    let total_spend = (50 * COIN_VALUE) + tx.fee + 1000;

    assert_eq!(balance, Scalar::from((100 * COIN_VALUE) - total_spend) * PC_GENS.B);
}


#[tokio::test]
async fn test_tx_deploy_contract() {
    let mut alice = Account::new();

    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);

    let tx = {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };

        let mut module = Module::new();
        module.add_chunk(Chunk::new());
        let data = TransactionTypeBuilder::DeployContract(DeployContractBuilder {
            module: module.to_hex(),
            invoke: None
        });
        let builder = TransactionBuilder::new(TxVersion::V2, alice.keypair.get_public_key().compress(), None, data, FeeBuilder::default());
        let estimated_size = builder.estimate_size();
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(estimated_size == tx.size(), "expected {} bytes got {} bytes", tx.size(), estimated_size);
        assert!(tx.to_bytes().len() == estimated_size);

        tx
    };

    let mut state = ChainState::new();

    // Create the chain state
    {
        let mut balances = HashMap::new();
        for (asset, balance) in &alice.balances {
            balances.insert(asset.clone(), balance.ciphertext.clone().take_ciphertext().unwrap());
        }
        state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    let hash = tx.hash();
    tx.verify(&hash, &mut state).await.unwrap();

    // Check Alice balance
    let balance = alice.keypair.decrypt_to_point(&state.accounts[&alice.keypair.get_public_key().compress()].balances[&TERMINOS_ASSET]);
    // 1 XEL for contract deploy, tx fee
    let total_spend = BURN_PER_CONTRACT + tx.fee;

    assert_eq!(balance, Scalar::from((100 * COIN_VALUE) - total_spend) * PC_GENS.B);
}

#[tokio::test]
async fn test_max_transfers() {
    let mut alice = Account::new();
    let mut bob = Account::new();

    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    bob.set_balance(TERMINOS_ASSET, 0);

    let tx = {
        let mut transfers = Vec::new();
        for _ in 0..MAX_TRANSFER_COUNT {
            transfers.push(TransferBuilder {
                amount: 1,
                destination: bob.address(),
                asset: TERMINOS_ASSET,
                extra_data: None,
                encrypt_extra_data: true,
            });
        }

        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };

        let data = TransactionTypeBuilder::Transfers(transfers);
        let builder = TransactionBuilder::new(TxVersion::V0, alice.keypair.get_public_key().compress(), None, data, FeeBuilder::default());
        let estimated_size = builder.estimate_size();
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(estimated_size == tx.size());
        assert!(tx.to_bytes().len() == estimated_size);

        tx
    };

    // Create the chain state
    let mut state = ChainState::new();

    // Alice
    {
        let mut balances = HashMap::new();
        for (asset, balance) in alice.balances {
            balances.insert(asset, balance.ciphertext.take_ciphertext().unwrap());
        }
        state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    // Bob
    {
        let mut balances = HashMap::new();
        for (asset, balance) in bob.balances {
            balances.insert(asset, balance.ciphertext.take_ciphertext().unwrap());
        }
        state.accounts.insert(bob.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }
    let hash = tx.hash();
    tx.verify(&hash, &mut state).await.unwrap();
}

#[tokio::test]
async fn test_multisig_setup() {
    let mut alice = Account::new();
    let mut bob = Account::new();
    let charlie = Account::new();

    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    bob.set_balance(TERMINOS_ASSET, 0);

    let tx = {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
    
        let data = TransactionTypeBuilder::MultiSig(MultiSigBuilder {
            threshold: 2,
            participants: IndexSet::from_iter(vec![bob.keypair.get_public_key().to_address(false), charlie.keypair.get_public_key().to_address(false)]),
        });
        let builder = TransactionBuilder::new(TxVersion::V1, alice.keypair.get_public_key().compress(), None, data, FeeBuilder::default());
        let estimated_size = builder.estimate_size();
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(estimated_size == tx.size());
        assert!(tx.to_bytes().len() == estimated_size);

        tx
    };

    let mut state = ChainState::new();

    // Create the chain state
    {
        let mut balances = HashMap::new();
        for (asset, balance) in alice.balances {
            balances.insert(asset, balance.ciphertext.take_ciphertext().unwrap());
        }
        state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    {
        let mut balances = HashMap::new();
        for (asset, balance) in bob.balances {
            balances.insert(asset, balance.ciphertext.take_ciphertext().unwrap());
        }
        state.accounts.insert(bob.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    let hash = tx.hash();
    tx.verify(&hash, &mut state).await.unwrap();

    assert!(state.multisig.contains_key(&alice.keypair.get_public_key().compress()));
}

#[tokio::test]
async fn test_multisig() {
    let mut alice = Account::new();
    let mut bob = Account::new();

    // Signers
    let charlie = Account::new();
    let dave = Account::new();

    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    bob.set_balance(TERMINOS_ASSET, 0);

    let tx = {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
    
        let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
            amount: 1,
            destination: bob.address(),
            asset: TERMINOS_ASSET,
            extra_data: None,
            encrypt_extra_data: true,
        }]);
        let builder = TransactionBuilder::new(TxVersion::V1, alice.keypair.get_public_key().compress(), Some(2), data, FeeBuilder::default());
        let mut tx = builder.build_unsigned(&mut state, &alice.keypair).unwrap();

        tx.sign_multisig(&charlie.keypair, 0);
        tx.sign_multisig(&dave.keypair, 1);

        tx.finalize(&alice.keypair)
    };

    // Create the chain state
    let mut state = ChainState::new();

    // Alice
    {
        let mut balances = HashMap::new();
        for (asset, balance) in alice.balances {
            balances.insert(asset, balance.ciphertext.take_ciphertext().unwrap());
        }
        state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    // Bob
    {
        let mut balances = HashMap::new();
        for (asset, balance) in bob.balances {
            balances.insert(asset, balance.ciphertext.take_ciphertext().unwrap());
        }

        state.accounts.insert(bob.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }

    state.multisig.insert(alice.keypair.get_public_key().compress(), MultiSigPayload {
        threshold: 2,
        participants: IndexSet::from_iter(vec![charlie.keypair.get_public_key().compress(), dave.keypair.get_public_key().compress()]),
    });

    let hash = tx.hash();
    tx.verify(&hash, &mut state).await.unwrap();
}

// Fee type and transaction type validation tests
#[test]
fn test_fee_type_enum() {
    use super::FeeType;
    
    // Test FeeType variants
    let tos_fee = FeeType::TOS;
    let energy_fee = FeeType::Energy;
    
    // Test is_energy method
    assert!(!tos_fee.is_energy());
    assert!(energy_fee.is_energy());
    
    // Test is_tos method
    assert!(tos_fee.is_tos());
    assert!(!energy_fee.is_tos());
    
    // Test equality
    assert_eq!(tos_fee, FeeType::TOS);
    assert_eq!(energy_fee, FeeType::Energy);
    assert_ne!(tos_fee, energy_fee);
}

#[test]
fn test_transaction_fee_type_methods() {
    use super::{FeeType, TransactionType, BurnPayload};
    
    // Test FeeType enum methods
    let tos_fee = FeeType::TOS;
    let energy_fee = FeeType::Energy;
    
    // Test is_energy method
    assert!(!tos_fee.is_energy());
    assert!(energy_fee.is_energy());
    
    // Test is_tos method
    assert!(tos_fee.is_tos());
    assert!(!energy_fee.is_tos());
    
    // Test transaction type validation
    let transfer_type = TransactionType::Transfers(vec![]);
    let burn_type = TransactionType::Burn(BurnPayload {
        asset: TERMINOS_ASSET,
        amount: 100,
    });
    
    // Energy fees should only be valid for transfers
    assert!(matches!(transfer_type, TransactionType::Transfers(_)));
    assert!(!matches!(burn_type, TransactionType::Transfers(_)));
    
    // Test fee type validation logic
    assert!(energy_fee.is_energy());
    assert!(tos_fee.is_tos());
}

#[test]
fn test_energy_fees_only_for_transfers() {
    use super::{FeeType, TransactionType, BurnPayload};
    
    // Test that energy fees are only valid for Transfer transactions
    let transfer_type = TransactionType::Transfers(vec![]);
    let burn_type = TransactionType::Burn(BurnPayload {
        asset: TERMINOS_ASSET,
        amount: 100,
    });
    
    // Energy fees should only be valid for transfers
    assert!(matches!(transfer_type, TransactionType::Transfers(_)));
    assert!(!matches!(burn_type, TransactionType::Transfers(_)));
    
    // FeeType validation logic
    let energy_fee = FeeType::Energy;
    let tos_fee = FeeType::TOS;
    
    // Energy fees are only valid for transfers
    assert!(energy_fee.is_energy());
    assert!(tos_fee.is_tos());
}

#[test]
fn test_transaction_builder_fee_type_validation() {
    let mut alice = Account::new();
    let bob = Account::new();
    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    
    // Test 1: Transfer transaction with Energy fees (should succeed)
    {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
        
        let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
            amount: 10,
            destination: bob.address(),
            asset: TERMINOS_ASSET,
            extra_data: None,
            encrypt_extra_data: true,
        }]);
        
        // Use energy fees for transfer (explicit)
        let builder = TransactionBuilder::new(
            TxVersion::V1, 
            alice.keypair.get_public_key().compress(), 
            None, 
            data, 
            FeeBuilder::Value(0)
        ).with_fee_type(FeeType::Energy);
        
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(tx.uses_energy_fees());
        assert!(matches!(tx.get_data(), TransactionType::Transfers(_)));
    }
    
    // Test 2: Burn transaction with TOS fees (should succeed)
    {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
        
        let data = TransactionTypeBuilder::Burn(BurnPayload {
            asset: TERMINOS_ASSET,
            amount: 10,
        });
        
        // Use TOS fees for burn (explicit)
        let builder = TransactionBuilder::new(
            TxVersion::V1, 
            alice.keypair.get_public_key().compress(), 
            None, 
            data, 
            FeeBuilder::Value(5000) // 0.00005 TOS fee
        ).with_fee_type(FeeType::TOS);
        
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(tx.uses_tos_fees());
        assert!(matches!(tx.get_data(), TransactionType::Burn(_)));
    }
}

#[test]
fn test_fee_type_serialization() {
    use super::FeeType;
    use crate::serializer::{Reader, Writer};
    
    // Test serialization and deserialization
    let fee_types = vec![FeeType::TOS, FeeType::Energy];
    
    for fee_type in fee_types {
        let mut buffer = Vec::new();
        let mut writer = Writer::new(&mut buffer);
        fee_type.write(&mut writer);
        
        let mut reader = Reader::new(&buffer);
        let deserialized = FeeType::read(&mut reader).unwrap();
        
        assert_eq!(fee_type, deserialized);
        assert_eq!(fee_type.size(), 1); // FeeType should be 1 byte
    }
}

#[test]
fn test_transaction_size_with_fee_type() {
    let mut alice = Account::new();
    let bob = Account::new();
    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    
    // Test that transaction size includes fee_type field
    let mut state = AccountStateImpl {
        balances: alice.balances.clone(),
        nonce: alice.nonce,
        reference: Reference {
            topoheight: 0,
            hash: Hash::zero(),
        },
    };
    
    let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
        amount: 10,
        destination: bob.address(),
        asset: TERMINOS_ASSET,
        extra_data: None,
        encrypt_extra_data: true,
    }]);
    
    // Test with TOS fees (explicit)
    let builder_tos = TransactionBuilder::new(
        TxVersion::V1, 
        alice.keypair.get_public_key().compress(), 
        None, 
        data.clone(), 
        FeeBuilder::Value(5000) // 0.00005 TOS fee
    ).with_fee_type(FeeType::TOS);
    let estimated_size_tos = builder_tos.estimate_size();
    let tx_tos = builder_tos.build(&mut state, &alice.keypair).unwrap();
    assert_eq!(estimated_size_tos, tx_tos.size());
    
    // Test with Energy fees (explicit)
    let builder_energy = TransactionBuilder::new(
        TxVersion::V1, 
        alice.keypair.get_public_key().compress(), 
        None, 
        data, 
        FeeBuilder::Value(0)
    ).with_fee_type(FeeType::Energy);
    let estimated_size_energy = builder_energy.estimate_size();
    let tx_energy = builder_energy.build(&mut state, &alice.keypair).unwrap();
    assert_eq!(estimated_size_energy, tx_energy.size());
    
    // Both should have the same size since fee_type is always present
    assert_eq!(tx_tos.size(), tx_energy.size());
}

#[test]
fn test_fee_type_default_behavior() {
    let mut alice = Account::new();
    let bob = Account::new();
    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    
    let mut state = AccountStateImpl {
        balances: alice.balances.clone(),
        nonce: alice.nonce,
        reference: Reference {
            topoheight: 0,
            hash: Hash::zero(),
        },
    };
    
    let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
        amount: 10,
        destination: bob.address(),
        asset: TERMINOS_ASSET,
        extra_data: None,
        encrypt_extra_data: true,
    }]);
    
    // Test default FeeBuilder (should use TOS fees)
    let builder = TransactionBuilder::new(
        TxVersion::V1, 
        alice.keypair.get_public_key().compress(), 
        None, 
        data, 
        FeeBuilder::default()
    );
    
    let tx = builder.build(&mut state, &alice.keypair).unwrap();
    assert!(tx.uses_tos_fees());
    assert_eq!(tx.get_fee_type(), &FeeType::TOS);
}

#[test]
fn test_transfer_default_fee_type_is_tos() {
    let mut alice = Account::new();
    let bob = Account::new();
    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    
    let mut state = AccountStateImpl {
        balances: alice.balances.clone(),
        nonce: alice.nonce,
        reference: Reference {
            topoheight: 0,
            hash: Hash::zero(),
        },
    };
    
    let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
        amount: 10,
        destination: bob.address(),
        asset: TERMINOS_ASSET,
        extra_data: None,
        encrypt_extra_data: true,
    }]);
    
    // Test default behavior (should use TOS fees regardless of fee amount)
    let builder = TransactionBuilder::new(
        TxVersion::V1, 
        alice.keypair.get_public_key().compress(), 
        None, 
        data, 
        FeeBuilder::Value(5) // Non-zero fee
    );
    
    let tx = builder.build(&mut state, &alice.keypair).unwrap();
    assert!(tx.uses_tos_fees());
    assert_eq!(tx.get_fee_type(), &FeeType::TOS);
    assert_eq!(tx.get_fee(), 5);
    
    // Test with zero fees (should still use TOS by default)
    let mut state2 = AccountStateImpl {
        balances: alice.balances.clone(),
        nonce: alice.nonce,
        reference: Reference {
            topoheight: 0,
            hash: Hash::zero(),
        },
    };
    
    let data2 = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
        amount: 10,
        destination: bob.address(),
        asset: TERMINOS_ASSET,
        extra_data: None,
        encrypt_extra_data: true,
    }]);
    
    let builder2 = TransactionBuilder::new(
        TxVersion::V1, 
        alice.keypair.get_public_key().compress(), 
        None, 
        data2, 
        FeeBuilder::Value(0) // Zero fee
    );
    
    let tx2 = builder2.build(&mut state2, &alice.keypair).unwrap();
    assert!(tx2.uses_tos_fees());
    assert_eq!(tx2.get_fee_type(), &FeeType::TOS);
    assert_eq!(tx2.get_fee(), 0);
}

#[test]
fn test_explicit_fee_type_behavior() {
    let mut alice = Account::new();
    let bob = Account::new();
    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    
    // Test 1: Transfer with explicit Energy fees (should succeed)
    {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
        
        let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
            amount: 10,
            destination: bob.address(),
            asset: TERMINOS_ASSET,
            extra_data: None,
            encrypt_extra_data: true,
        }]);
        
        let builder = TransactionBuilder::new(
            TxVersion::V1, 
            alice.keypair.get_public_key().compress(), 
            None, 
            data, 
            FeeBuilder::Value(0)
        ).with_fee_type(FeeType::Energy);
        
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(tx.uses_energy_fees());
        assert_eq!(tx.get_fee_type(), &FeeType::Energy);
        assert_eq!(tx.get_fee(), 0);
    }
    
    // Test 2: Transfer with explicit TOS fees (should succeed)
    {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
        
        let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
            amount: 10,
            destination: bob.address(),
            asset: TERMINOS_ASSET,
            extra_data: None,
            encrypt_extra_data: true,
        }]);
        
        let builder = TransactionBuilder::new(
            TxVersion::V1, 
            alice.keypair.get_public_key().compress(), 
            None, 
            data, 
            FeeBuilder::Value(5000) // 0.00005 TOS fee
        ).with_fee_type(FeeType::TOS);
        
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(tx.uses_tos_fees());
        assert_eq!(tx.get_fee_type(), &FeeType::TOS);
        assert_eq!(tx.get_fee(), 5000);
    }
    
    // Test 3: Burn with explicit TOS fees (should succeed)
    {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
        
        let data = TransactionTypeBuilder::Burn(BurnPayload {
            asset: TERMINOS_ASSET,
            amount: 10,
        });
        
        let builder = TransactionBuilder::new(
            TxVersion::V1, 
            alice.keypair.get_public_key().compress(), 
            None, 
            data, 
            FeeBuilder::Value(5000) // 0.00005 TOS fee
        ).with_fee_type(FeeType::TOS);
        
        let tx = builder.build(&mut state, &alice.keypair).unwrap();
        assert!(tx.uses_tos_fees());
        assert_eq!(tx.get_fee_type(), &FeeType::TOS);
        assert!(matches!(tx.get_data(), TransactionType::Burn(_)));
    }
}

#[test]
fn test_energy_fees_validation_for_non_transfer() {
    let mut alice = Account::new();
    alice.set_balance(TERMINOS_ASSET, 100 * COIN_VALUE);
    
    // Test: Burn transaction with explicit Energy fees (should fail)
    {
        let mut state = AccountStateImpl {
            balances: alice.balances.clone(),
            nonce: alice.nonce,
            reference: Reference {
                topoheight: 0,
                hash: Hash::zero(),
            },
        };
        
        let data = TransactionTypeBuilder::Burn(BurnPayload {
            asset: TERMINOS_ASSET,
            amount: 10,
        });
        
        let builder = TransactionBuilder::new(
            TxVersion::V1, 
            alice.keypair.get_public_key().compress(), 
            None, 
            data, 
            FeeBuilder::Value(0)
        ).with_fee_type(FeeType::Energy);
        
        // This should fail because Energy fees are not allowed for non-transfer transactions
        let result = builder.build(&mut state, &alice.keypair);
        assert!(result.is_err());
        
        // Check the specific error type
        match result {
            Err(GenerationError::EnergyFeesNotAllowedForNonTransfer) => {
                // Expected error
            }
            _ => panic!("Expected EnergyFeesNotAllowedForNonTransfer error"),
        }
    }
}

#[async_trait]
impl<'a> BlockchainVerificationState<'a, ()> for ChainState {

    /// Pre-verify the TX
    async fn pre_verify_tx<'b>(
        &'b mut self,
        _: &Transaction,
    ) -> Result<(), ()> {
        Ok(())
    }

    /// Get the balance ciphertext for a receiver account
    async fn get_receiver_balance<'b>(
        &'b mut self,
        account: Cow<'a, PublicKey>,
        asset: Cow<'a, Hash>,
    ) -> Result<&'b mut Ciphertext, ()> {
        self.accounts.get_mut(&account).and_then(|account| account.balances.get_mut(&asset)).ok_or(())
    }

    /// Get the balance ciphertext used for verification of funds for the sender account
    async fn get_sender_balance<'b>(
        &'b mut self,
        account: &'a PublicKey,
        asset: &'a Hash,
        _: &Reference,
    ) -> Result<&'b mut Ciphertext, ()> {
        self.accounts.get_mut(account).and_then(|account| account.balances.get_mut(asset)).ok_or(())
    }

    /// Apply new output to a sender account
    async fn add_sender_output(
        &mut self,
        _: &'a PublicKey,
        _: &'a Hash,
        _: Ciphertext,
    ) -> Result<(), ()> {
        Ok(())
    }

    /// Get the nonce of an account
    async fn get_account_nonce(
        &mut self,
        account: &'a PublicKey
    ) -> Result<Nonce, ()> {
        self.accounts.get(account).map(|account| account.nonce).ok_or(())
    }

    /// Apply a new nonce to an account
    async fn update_account_nonce(
        &mut self,
        account: &'a PublicKey,
        new_nonce: Nonce
    ) -> Result<(), ()> {
        self.accounts.get_mut(account).map(|account| account.nonce = new_nonce).ok_or(())
    }

    fn get_block_version(&self) -> BlockVersion {
        BlockVersion::V0
    }

    async fn set_multisig_state(
        &mut self,
        account: &'a PublicKey,
        multisig: &MultiSigPayload
    ) -> Result<(), ()> {
        self.multisig.insert(account.clone(), multisig.clone());
        Ok(())
    }

    async fn get_multisig_state(
        &mut self,
        account: &'a PublicKey
    ) -> Result<Option<&MultiSigPayload>, ()> {
        Ok(self.multisig.get(account))
    }

    async fn get_environment(&mut self) -> Result<&Environment, ()> {
        Ok(&self.env)
    }

    async fn set_contract_module(
        &mut self,
        hash: &'a Hash,
        module: &'a Module
    ) -> Result<(), ()> {
        self.contracts.insert(hash.clone(), module.clone());
        Ok(())
    }

    async fn load_contract_module(
        &mut self,
        hash: &'a Hash
    ) -> Result<bool, ()> {
        Ok(self.contracts.contains_key(hash))
    }

    async fn get_contract_module_with_environment(
        &self,
        contract: &'a Hash
    ) -> Result<(&Module, &Environment), ()> {
        let module = self.contracts.get(contract).ok_or(())?;
        Ok((module, &self.env))
    }
}

impl FeeHelper for AccountStateImpl {
    type Error = ();

    fn account_exists(&self, _: &PublicKey) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

impl AccountState for AccountStateImpl {
    fn is_mainnet(&self) -> bool {
        false
    }

    fn get_account_balance(&self, asset: &Hash) -> Result<u64, Self::Error> {
        self.balances.get(asset).map(|balance| balance.balance).ok_or(())
    }

    fn get_account_ciphertext(&self, asset: &Hash) -> Result<CiphertextCache, Self::Error> {
        self.balances.get(asset).map(|balance| balance.ciphertext.clone()).ok_or(())
    }

    fn get_reference(&self) -> Reference {
        self.reference.clone()
    }

    fn update_account_balance(&mut self, asset: &Hash, balance: u64, ciphertext: Ciphertext) -> Result<(), Self::Error> {
        self.balances.insert(asset.clone(), Balance {
            balance,
            ciphertext: CiphertextCache::Decompressed(ciphertext),
        });
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

#[tokio::test]
async fn test_tos_transfer_with_tos_fees_balance_verification() {
    let mut alice = Account::new();
    let mut bob = Account::new();
    
    // Create a dummy Energy asset hash
    let energy_asset = Hash::from_bytes(&[1u8; 32]).unwrap();
    
    // Set initial balances
    // Alice: 10 TOS, 1000 Energy
    alice.set_balance(TERMINOS_ASSET, 10 * COIN_VALUE);
    alice.set_balance(energy_asset.clone(), 1000); // Energy asset (using a dummy hash)
    
    // Bob: 1 TOS, 2000 Energy
    bob.set_balance(TERMINOS_ASSET, 1 * COIN_VALUE);
    bob.set_balance(energy_asset.clone(), 2000); // Energy asset
    
    // Verify initial balances
    assert_eq!(alice.balances[&TERMINOS_ASSET].balance, 10 * COIN_VALUE);
    assert_eq!(alice.balances[&energy_asset].balance, 1000);
    assert_eq!(bob.balances[&TERMINOS_ASSET].balance, 1 * COIN_VALUE);
    assert_eq!(bob.balances[&energy_asset].balance, 2000);
    
    // Create transfer transaction: Alice sends 1 TOS to Bob with TOS fees
    let mut state = AccountStateImpl {
        balances: alice.balances.clone(),
        nonce: alice.nonce,
        reference: Reference {
            topoheight: 0,
            hash: Hash::zero(),
        },
    };
    
    let data = TransactionTypeBuilder::Transfers(vec![TransferBuilder {
        amount: 1 * COIN_VALUE, // 1 TOS
        destination: bob.address(),
        asset: TERMINOS_ASSET,
        extra_data: None,
        encrypt_extra_data: true,
    }]);
    
    let builder = TransactionBuilder::new(
        TxVersion::V1, 
        alice.keypair.get_public_key().compress(), 
        None, 
        data, 
        FeeBuilder::Value(5000) // 0.00005 TOS fee (5,000 atomic units)
    ).with_fee_type(FeeType::TOS);
    
    let tx = builder.build(&mut state, &alice.keypair).unwrap();
    
    // Verify transaction properties
    assert!(tx.uses_tos_fees());
    assert_eq!(tx.get_fee_type(), &FeeType::TOS);
    assert_eq!(tx.get_fee(), 5000); // 0.00005 TOS fee (5,000 atomic units)
    
    // Create chain state for verification
    let mut chain_state = ChainState::new();
    
    // Add Alice's balances to chain state
    {
        let mut balances = HashMap::new();
        for (asset, balance) in &alice.balances {
            balances.insert(asset.clone(), balance.ciphertext.clone().take_ciphertext().unwrap());
        }
        chain_state.accounts.insert(alice.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: alice.nonce,
        });
    }
    
    // Add Bob's balances to chain state
    {
        let mut balances = HashMap::new();
        for (asset, balance) in &bob.balances {
            balances.insert(asset.clone(), balance.ciphertext.clone().take_ciphertext().unwrap());
        }
        chain_state.accounts.insert(bob.keypair.get_public_key().compress(), AccountChainState {
            balances,
            nonce: bob.nonce,
        });
    }
    
    // Verify the transaction
    let hash = tx.hash();
    tx.verify(&hash, &mut chain_state).await.unwrap();
    
    // Check final balances after transfer
    // Alice should have: 10 - 1 (transfer) - 0.00005 (fee) = 8.99995 TOS
    let alice_tos_balance = alice.keypair.decrypt_to_point(&chain_state.accounts[&alice.keypair.get_public_key().compress()].balances[&TERMINOS_ASSET]);
    let expected_alice_tos = Scalar::from((10u64 * COIN_VALUE) - (1u64 * COIN_VALUE) - 5000u64) * PC_GENS.B;
    assert_eq!(alice_tos_balance, expected_alice_tos);
    
    // Alice's Energy should remain unchanged: 1000
    let alice_energy_balance = alice.keypair.decrypt_to_point(&chain_state.accounts[&alice.keypair.get_public_key().compress()].balances[&energy_asset]);
    let expected_alice_energy = Scalar::from(1000u64) * PC_GENS.B;
    assert_eq!(alice_energy_balance, expected_alice_energy);
    
    // Bob should have: 1 + 1 = 2 TOS
    let bob_tos_balance = bob.keypair.decrypt_to_point(&chain_state.accounts[&bob.keypair.get_public_key().compress()].balances[&TERMINOS_ASSET]);
    let expected_bob_tos = Scalar::from(2u64 * COIN_VALUE) * PC_GENS.B;
    assert_eq!(bob_tos_balance, expected_bob_tos);
    
    // Bob's Energy should remain unchanged: 2000
    let bob_energy_balance = bob.keypair.decrypt_to_point(&chain_state.accounts[&bob.keypair.get_public_key().compress()].balances[&energy_asset]);
    let expected_bob_energy = Scalar::from(2000u64) * PC_GENS.B;
    assert_eq!(bob_energy_balance, expected_bob_energy);
    
    println!("Transfer verification successful!");
    println!("Alice final TOS balance: 8.99995 TOS (10 - 1 transfer - 0.00005 fee)");
    println!("Alice final Energy balance: 1000 (unchanged)");
    println!("Bob final TOS balance: 2 TOS (1 + 1 transfer)");
    println!("Bob final Energy balance: 2000 (unchanged)");
}