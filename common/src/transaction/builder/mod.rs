//! This file represents the transactions without the proofs
//! Not really a 'builder' per say
//! Intended to be used when creating a transaction before making the associated proofs and signature
mod state;
mod fee;
mod unsigned;
mod payload;

pub use state::AccountState;
pub use fee::{FeeHelper, FeeBuilder};
pub use unsigned::UnsignedTransaction;

use indexmap::{IndexMap, IndexSet};
use merlin::Transcript;
use bulletproofs::RangeProof;
use curve25519_dalek::Scalar;
use serde::{Deserialize, Serialize};
use terminos_vm::Module;
use std::{
    collections::{HashMap, HashSet},
    iter,
};
use crate::{
    config::{BURN_PER_CONTRACT, MAX_GAS_USAGE_PER_TX, TERMINOS_ASSET},
    crypto::{
        elgamal::{
            Ciphertext,
            CompressedPublicKey,
            DecryptHandle,
            KeyPair,
            PedersenCommitment,
            PedersenOpening,
            PublicKey,
            RISTRETTO_COMPRESSED_SIZE,
            SCALAR_SIZE
        },
        proofs::{
            CiphertextValidityProof,
            CommitmentEqProof,
            ProofGenerationError,
            BP_GENS,
            PC_GENS,
            BULLET_PROOF_SIZE,
        },
        Hash,
        ProtocolTranscript,
        HASH_SIZE,
        SIGNATURE_SIZE
    },
    serializer::Serializer,
    utils::{calculate_tx_fee, calculate_energy_fee}
};
use thiserror::Error;
use super::{
    extra_data::{
        ExtraData,
        ExtraDataType,
        PlaintextData,
        UnknownExtraDataFormat
    },
    BurnPayload,
    ContractDeposit,
    DeployContractPayload,
    EnergyPayload,
    FeeType,
    InvokeConstructorPayload,
    InvokeContractPayload,
    MultiSigPayload,
    Role,
    SourceCommitment,
    Transaction,
    TransactionType,
    TransferPayload,
    TxVersion,
    EXTRA_DATA_LIMIT_SIZE,
    EXTRA_DATA_LIMIT_SUM_SIZE,
    MAX_MULTISIG_PARTICIPANTS,
    MAX_TRANSFER_COUNT
};


pub use payload::*;

#[derive(Error, Debug, Clone)]
pub enum GenerationError<T> {
    #[error("Error in the state: {0}")]
    State(T),
    #[error("Invalid constructor invoke on deploy")]
    InvalidConstructorInvoke,
    #[error("No contract key provided for private deposits")]
    MissingContractKey,
    #[error("Empty transfers")]
    EmptyTransfers,
    #[error("Max transfer count reached")]
    MaxTransferCountReached,
    #[error("Sender is receiver")]
    SenderIsReceiver,
    #[error("Extra data too large")]
    ExtraDataTooLarge,
    #[error("Encrypted extra data is too large, we got {0} bytes, limit is {1} bytes")]
    EncryptedExtraDataTooLarge(usize, usize),
    #[error("Address is not on the same network as us")]
    InvalidNetwork,
    #[error("Extra data was provied with an integrated address")]
    ExtraDataAndIntegratedAddress,
    #[error("Proof generation error: {0}")]
    Proof(#[from] ProofGenerationError),
    #[error("Invalid multisig participants count")]
    MultiSigParticipants,
    #[error("Invalid multisig threshold")]
    MultiSigThreshold,
    #[error("Cannot contains yourself in the multisig participants")]
    MultiSigSelfParticipant,
    #[error("Burn amount is zero")]
    BurnZero,
    #[error("Deposit amount is zero")]
    DepositZero,
    #[error("Invalid module hexadecimal")]
    InvalidModule,
    #[error("Configured max gas is above the network limit")]
    MaxGasReached,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TransactionTypeBuilder {
    Transfers(Vec<TransferBuilder>),
    // We can use the same as final transaction
    Burn(BurnPayload),
    MultiSig(MultiSigBuilder),
    InvokeContract(InvokeContractBuilder),
    DeployContract(DeployContractBuilder),
    Energy(EnergyBuilder),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransactionBuilder {
    version: TxVersion,
    source: CompressedPublicKey,
    required_thresholds: Option<u8>,
    data: TransactionTypeBuilder,
    fee_builder: FeeBuilder,
    /// Optional fee type (TOS or Energy). If None, use default logic.
    fee_type: Option<super::FeeType>,
}

impl TransactionBuilder {
    pub fn new(version: TxVersion, source: CompressedPublicKey, required_thresholds: Option<u8>, data: TransactionTypeBuilder, fee_builder: FeeBuilder) -> Self {
        Self {
            version,
            source,
            required_thresholds,
            data,
            fee_builder,
            fee_type: None,
        }
    }
    /// Set the fee type for this transaction
    pub fn with_fee_type(mut self, fee_type: super::FeeType) -> Self {
        self.fee_type = Some(fee_type);
        self
    }

    /// Create a transaction builder with energy-based fees (fee = 0)
    /// Energy can only be used for Transfer transactions to provide free TOS and other token transfers
    pub fn with_energy_fees(mut self) -> Self {
        self.fee_builder = FeeBuilder::Value(0);
        self.fee_type = Some(FeeType::Energy);
        self
    }

    /// Create a transaction builder with TOS-based fees
    pub fn with_tos_fees(mut self, fee: u64) -> Self {
        self.fee_builder = FeeBuilder::Value(fee);
        self.fee_type = Some(FeeType::TOS);
        self
    }

    /// Estimate by hand the bytes size of a final TX
    // Returns bytes size and transfers count
    pub fn estimate_size(&self) -> usize {
        let assets_used = self.data.used_assets().len();
        // Version byte
        let mut size = 1
        // Source Public Key
        + self.source.size()
        // Transaction type byte
        + 1
        // Fee u64
        + 8
        // Fee type byte (TOS or Energy)
        + 1
        // Nonce u64
        + 8
        // Reference (hash, topo)
        + HASH_SIZE + 8
        // Commitments byte length
        + 1
        // We have one source commitment per asset spent
        // assets * (commitment, asset, proof)
        + assets_used * (RISTRETTO_COMPRESSED_SIZE + HASH_SIZE + (RISTRETTO_COMPRESSED_SIZE * 3 + SCALAR_SIZE * 3))
        // Signature
        + SIGNATURE_SIZE
        // 1 for optional multisig bool (always included for T0)
        + 1
        ;

        if let Some(threshold) = self.required_thresholds {
            // 1 for Multisig participants count byte
            size += 1 + (threshold as usize * (SIGNATURE_SIZE + 1))
        }

        let mut commitments_count = 0;
        match &self.data {
            TransactionTypeBuilder::Transfers(transfers) => {
                // Transfers count byte
                size += 1;
                for transfer in transfers {
                    size += transfer.asset.size()
                    + transfer.destination.get_public_key().size()
                    // Commitment, sender handle, receiver handle
                    + (RISTRETTO_COMPRESSED_SIZE * 3)
                    // Ct Validity Proof
                    + (RISTRETTO_COMPRESSED_SIZE * 2 + SCALAR_SIZE * 2)
                    // Extra data byte flag
                    + 1;

                    // Always include Y_2 for T0
                    size += RISTRETTO_COMPRESSED_SIZE;

                    if let Some(extra_data) = transfer.extra_data.as_ref().or(transfer.destination.get_extra_data()) {
                        // Always use ExtraDataType for T0
                        size += ExtraDataType::estimate_size(extra_data, transfer.encrypt_extra_data);
                    }
                }
                commitments_count = transfers.len();
            }
            TransactionTypeBuilder::Burn(payload) => {
                // Payload size
                size += payload.size();
            },
            TransactionTypeBuilder::MultiSig(payload) => {
                // Payload size
                size += payload.threshold.size() + 1 + (payload.participants.len() * RISTRETTO_COMPRESSED_SIZE);
            },
            TransactionTypeBuilder::InvokeContract(payload) => {
                let payload_size = payload.contract.size()
                + payload.max_gas.size()
                + payload.chunk_id.size()
                + 1 // byte for params len
                // 4 is for the compressed constant len
                + payload.parameters.iter().map(|param| 4 + param.size()).sum::<usize>();

                size += payload_size;

                let (commitments, deposits_size) = self.estimate_deposits_size(&payload.deposits);
                commitments_count += commitments;
                size += deposits_size;
            },
            TransactionTypeBuilder::DeployContract(payload) => {
                // Module is in hex format, so we need to divide by 2 for its bytes size
                // + 1 for the invoke option
                size += payload.module.len() / 2 + 1;
                if let Some(invoke) = payload.invoke.as_ref() {
                    let (commitments, deposits_size) = self.estimate_deposits_size(&invoke.deposits);

                    commitments_count += commitments;
                    size += deposits_size + invoke.max_gas.size();
                }
            },
            TransactionTypeBuilder::Energy(payload) => {
                // Convert EnergyBuilder to EnergyPayload for size calculation
                let energy_payload = match payload {
                    EnergyBuilder { amount, is_freeze: true, freeze_duration: Some(duration) } => {
                        EnergyPayload::FreezeTos {
                            amount: *amount,
                            duration: duration.clone(),
                        }
                    },
                    EnergyBuilder { amount, is_freeze: false, freeze_duration: None } => {
                        EnergyPayload::UnfreezeTos {
                            amount: *amount,
                        }
                    },
                    _ => {
                        // This should not happen due to validation, but handle gracefully
                        EnergyPayload::UnfreezeTos { amount: 0 }
                    }
                };
                
                // Payload size
                size += energy_payload.size();
            }
        };

        // Range Proof
        let lg_n = (BULLET_PROOF_SIZE * (commitments_count + assets_used)).next_power_of_two().trailing_zeros() as usize;
        // Fixed size of the range proof
        size += RISTRETTO_COMPRESSED_SIZE * 4 + SCALAR_SIZE * 3
        // u16 bytes length
        + 2
        // Inner Product Proof
        // scalars
        + SCALAR_SIZE * 2
        // G_vec len
        + 2 * RISTRETTO_COMPRESSED_SIZE * lg_n;

        size
    }

    fn estimate_deposits_size(&self, deposits: &IndexMap<Hash, ContractDepositBuilder>) -> (usize, usize) {
        let mut commitments_count = 0;
        // Init to 1 for the deposits len
        let mut size = 1;
        for (asset, deposit) in deposits {
            // 1 is for the deposit variant
            size += asset.size() + 1;
            if deposit.private {
                // Commitment, sender handle, receiver handle
                size += RISTRETTO_COMPRESSED_SIZE * 3;
                // Ct validity proof
                size += RISTRETTO_COMPRESSED_SIZE * 3 + SCALAR_SIZE * 2;

                // Increment the commitments count
                // Each deposit is a commitment
                commitments_count += 1;
            } else {
                size += deposit.amount.size();
            }
        }

        (commitments_count, size)
    }

    // Estimate the fees for this TX
    pub fn estimate_fees<B: FeeHelper>(&self, state: &mut B) -> Result<u64, GenerationError<B::Error>> {
        let calculated_fee = match self.fee_builder {
            // If the value is set, use it
            FeeBuilder::Value(value) => value,
            _ => {
                // Compute the size and transfers count
                let size = self.estimate_size();
                let (transfers, new_addresses) = if let TransactionTypeBuilder::Transfers(transfers) = &self.data {
                    let mut new_addresses = 0;
                    for transfer in transfers {
                        if !state.account_exists(&transfer.destination.get_public_key()).map_err(GenerationError::State)? {
                            new_addresses += 1;
                        }
                    }

                    (transfers.len(), new_addresses)
                } else {
                    (0, 0)
                };

                // Check if we should use energy fees for transfer transactions
                let expected_fee = if let Some(ref fee_type) = self.fee_type {
                    if *fee_type == FeeType::Energy && matches!(self.data, TransactionTypeBuilder::Transfers(_)) {
                        // Use energy fee calculation for transfer transactions
                        let energy_fee = calculate_energy_fee(size, transfers, new_addresses);
                        println!("[ESTIMATE DEBUG] Energy fee calculation: size={}, transfers={}, new_addresses={}, energy_fee={}", size, transfers, new_addresses, energy_fee);
                        energy_fee
                    } else {
                        // Use regular TOS fee calculation
                        calculate_tx_fee(size, transfers, new_addresses, self.required_thresholds.unwrap_or(0) as usize)
                    }
                } else {
                    // Default to TOS fees
                    calculate_tx_fee(size, transfers, new_addresses, self.required_thresholds.unwrap_or(0) as usize)
                };
                
                match self.fee_builder {
                    FeeBuilder::Multiplier(multiplier) => (expected_fee as f64 * multiplier) as u64,
                    FeeBuilder::Boost(boost) => expected_fee + boost,
                    _ => expected_fee,
                }
            },
        };

        Ok(calculated_fee)
    }

    // Compute the new source ciphertext
    fn get_new_source_ct(
        &self,
        mut ct: Ciphertext,
        fee: u64,
        asset: &Hash,
        transfers: &[TransferWithCommitment],
        deposits: &HashMap<Hash, DepositWithCommitment>,
    ) -> Ciphertext {
        if asset == &TERMINOS_ASSET {
            // Fees are applied to the native blockchain asset only.
            ct -= Scalar::from(fee);
        }

        match &self.data {
            TransactionTypeBuilder::Transfers(_) => {
                for transfer in transfers {
                    if &transfer.inner.asset == asset {
                        ct -= transfer.get_ciphertext(Role::Sender);
                    }
                }
            }
            TransactionTypeBuilder::Burn(payload) => {
                if *asset == payload.asset {
                    ct -= Scalar::from(payload.amount)
                }
            },
            TransactionTypeBuilder::MultiSig(_) => {},
            TransactionTypeBuilder::InvokeContract(payload) => {
                if let Some(deposit) = payload.deposits.get(asset) {
                    if deposit.private {
                        if let Some(deposit) = deposits.get(asset) {
                            ct -= deposit.get_ciphertext(Role::Sender);
                        }
                    } else {
                        ct -= Scalar::from(deposit.amount);
                    }
                }

                if *asset == TERMINOS_ASSET {
                    ct -= Scalar::from(payload.max_gas);
                }
            },
            TransactionTypeBuilder::DeployContract(payload) => {
                if let Some(invoke) = payload.invoke.as_ref() {
                    if let Some(deposit) = invoke.deposits.get(asset) {
                        if deposit.private {
                            if let Some(deposit) = deposits.get(asset) {
                                ct -= deposit.get_ciphertext(Role::Sender);
                            }
                        } else {
                            ct -= Scalar::from(deposit.amount);
                        }
                    }

                    if *asset == TERMINOS_ASSET {
                        ct -= Scalar::from(invoke.max_gas);
                    }
                }

                if *asset == TERMINOS_ASSET {
                    ct -= Scalar::from(BURN_PER_CONTRACT);
                }
            },
            TransactionTypeBuilder::Energy(payload) => {
                if *asset == TERMINOS_ASSET {
                    ct -= Scalar::from(payload.amount);
                }
            }
        }

        ct
    }

    /// Compute the full cost of the transaction
    pub fn get_transaction_cost(&self, fee: u64, asset: &Hash) -> u64 {
        let mut cost = 0;

        // Check if we should apply fees to TOS balance
        let should_apply_fees = if let Some(ref fee_type) = self.fee_type {
            // For Energy fees, we don't deduct from TOS balance
            // Energy is consumed separately from the account's energy resource
            *fee_type == FeeType::TOS
        } else {
            // Default to TOS fees
            true
        };

        if *asset == TERMINOS_ASSET && should_apply_fees {
            // Fees are applied to the native blockchain asset only.
            cost += fee;
        }

        match &self.data {
            TransactionTypeBuilder::Transfers(transfers) => {
                for transfer in transfers {
                    if &transfer.asset == asset {
                        cost += transfer.amount;
                    }
                }
            }
            TransactionTypeBuilder::Burn(payload) => {
                if *asset == payload.asset {
                    cost += payload.amount
                }
            },
            TransactionTypeBuilder::MultiSig(_) => {},
            TransactionTypeBuilder::InvokeContract(payload) => {
                if let Some(deposit) = payload.deposits.get(asset) {
                    cost += deposit.amount;
                }

                if *asset == TERMINOS_ASSET {
                    cost += payload.max_gas;
                }
            },
            TransactionTypeBuilder::DeployContract(payload) => {
                if *asset == TERMINOS_ASSET {
                    cost += BURN_PER_CONTRACT;
                }

                if let Some(invoke) = payload.invoke.as_ref() {
                    if let Some(deposit) = invoke.deposits.get(asset) {
                        cost += deposit.amount;
                    }

                    if *asset == TERMINOS_ASSET {
                        cost += invoke.max_gas;
                    }
                }
            },
            TransactionTypeBuilder::Energy(payload) => {
                if *asset == TERMINOS_ASSET {
                    cost += payload.amount;
                }
            }
        }

        cost
    }

    // Build the deposits commitments for the contract
    fn build_deposits_commitments<E>(
        deposits: &IndexMap<Hash, ContractDepositBuilder>,
        public_key: &PublicKey,
        contract_key: &Option<PublicKey>,
    ) -> Result<HashMap<Hash, DepositWithCommitment>, GenerationError<E>> {
        let mut deposits_commitments = HashMap::new();
        for (asset, deposit) in deposits.iter() {
            if deposit.private {
                let amount_opening = PedersenOpening::generate_new();
                let commitment = PedersenCommitment::new_with_opening(deposit.amount, &amount_opening);
                let sender_handle = public_key.decrypt_handle(&amount_opening);
                let receiver_handle = contract_key
                    .as_ref()
                    .ok_or(GenerationError::MissingContractKey)?
                    .decrypt_handle(&amount_opening);

                deposits_commitments.insert(asset.clone(), DepositWithCommitment {
                    amount: deposit.amount,
                    commitment,
                    sender_handle,
                    receiver_handle,
                    amount_opening,
                });
                todo!("support private deposits")
            } else {
                if deposit.amount == 0 {
                    return Err(GenerationError::DepositZero);
                }
            }
        }

        Ok(deposits_commitments)
    }

    // Finalize the deposits commitments
    // Public & private variants are built here
    fn finalize_deposits_commitments(
        transcript: &mut Transcript,
        range_proof_values: &mut Vec<u64>,
        range_proof_openings: &mut Vec<Scalar>,
        payload_deposits: &mut IndexMap<Hash, ContractDepositBuilder>,
        deposits_commitments: HashMap<Hash, DepositWithCommitment>,
        source_keypair: &KeyPair,
        contract_key: &Option<PublicKey>,
    ) -> IndexMap<Hash, ContractDeposit> {
        range_proof_openings.reserve(deposits_commitments.len());
        range_proof_values.reserve(deposits_commitments.len());

        // Build the private deposits
        let mut deposits = deposits_commitments
            .into_iter()
            .map(|(asset, deposit)| {
                let commitment = deposit.commitment.compress();
                let sender_handle = deposit.sender_handle.compress();
                let receiver_handle = deposit.receiver_handle.compress();

                transcript.deposit_proof_domain_separator();
                transcript.append_hash(b"deposit_asset", &asset);
                transcript.append_commitment(b"deposit_commitment", &commitment);
                transcript.append_handle(b"deposit_sender_handle", &sender_handle);
                transcript.append_handle(b"deposit_receiver_handle", &receiver_handle);

                let ct_validity_proof = CiphertextValidityProof::new(
                    contract_key.as_ref().expect("Contract key is required"),
                    Some(source_keypair.get_public_key()),
                    deposit.amount,
                    &deposit.amount_opening,
                    transcript,
                );

                range_proof_values.push(deposit.amount);
                range_proof_openings.push(deposit.amount_opening.as_scalar());

                (
                    asset,
                    ContractDeposit::Private { commitment, sender_handle, receiver_handle, ct_validity_proof }
                )
            }).collect::<IndexMap<_, _>>();

            // Now build the public ones
            for (asset, deposit) in payload_deposits.drain(..).filter(|(_, deposit)| !deposit.private) {
                transcript.deposit_proof_domain_separator();
                transcript.append_hash(b"deposit_asset", &asset);
                transcript.append_u64(b"deposit_plain", deposit.amount);

                deposits.insert(asset, ContractDeposit::Public(deposit.amount));
            }

            deposits
    }

    pub fn build<B: AccountState>(
        self,
        state: &mut B,
        source_keypair: &KeyPair,
    ) -> Result<Transaction, GenerationError<B::Error>> where for<'a> <B as FeeHelper>::Error: std::convert::From<&'a str> {
        let unsigned = self.build_unsigned(state, source_keypair)?;
        Ok(unsigned.finalize(source_keypair))
    }

    pub fn build_unsigned<B: AccountState>(
        mut self,
        state: &mut B,
        source_keypair: &KeyPair,
    ) -> Result<UnsignedTransaction, GenerationError<B::Error>> where <B as FeeHelper>::Error: for<'a> std::convert::From<&'a str> {
        // Compute the fees
        let fee = self.estimate_fees(state)?;

        // Get the nonce
        let nonce = state.get_nonce().map_err(GenerationError::State)?;
        state.update_nonce(nonce + 1).map_err(GenerationError::State)?;

        // 0.a Create the commitments

        // Data is mutable only to extract extra data
        let mut transfers_commitments = Vec::new();
        let mut deposits_commitments = HashMap::new();
        match &mut self.data {
            TransactionTypeBuilder::Transfers(transfers) => {
                if transfers.len() == 0 {
                    return Err(GenerationError::EmptyTransfers);
                }

                if transfers.len() > MAX_TRANSFER_COUNT {
                    return Err(GenerationError::MaxTransferCountReached);
                }
    
                let mut extra_data_size = 0;
                for transfer in transfers.iter_mut() {
                    if *transfer.destination.get_public_key() == self.source {
                        return Err(GenerationError::SenderIsReceiver);
                    }
    
                    if state.is_mainnet() != transfer.destination.is_mainnet() {
                        return Err(GenerationError::InvalidNetwork);
                    }
    
                    // Either extra data provided or an integrated address, not both
                    if transfer.extra_data.is_some() && !transfer.destination.is_normal() {
                        return Err(GenerationError::ExtraDataAndIntegratedAddress);
                    }
    
                    // Set the integrated data as extra data
                    if let Some(extra_data) = transfer.destination.extract_data_only() {
                        transfer.extra_data = Some(extra_data);
                    }
    
                    if let Some(extra_data) = &transfer.extra_data {
                        let size = extra_data.size();
                        if size > EXTRA_DATA_LIMIT_SIZE {
                            return Err(GenerationError::ExtraDataTooLarge);
                        }
                        extra_data_size += size;
                    }
                }
    
                if extra_data_size > EXTRA_DATA_LIMIT_SUM_SIZE {
                    return Err(GenerationError::ExtraDataTooLarge);
                }
    
                transfers_commitments = transfers
                    .iter()
                    .map(|transfer| {
                        let destination = transfer
                            .destination
                            .get_public_key()
                            .decompress()
                            .map_err(|err| GenerationError::Proof(err.into()))?;
    
                        let amount_opening = PedersenOpening::generate_new();
                        let commitment =
                            PedersenCommitment::new_with_opening(transfer.amount, &amount_opening);
                        let sender_handle =
                            source_keypair.get_public_key().decrypt_handle(&amount_opening);
                        let receiver_handle = destination.decrypt_handle(&amount_opening);
    
                        Ok(TransferWithCommitment {
                            inner: transfer.clone(),
                            commitment,
                            sender_handle,
                            receiver_handle,
                            destination,
                            amount_opening,
                        })
                    })
                    .collect::<Result<Vec<_>, GenerationError<B::Error>>>()?;
            },
            TransactionTypeBuilder::InvokeContract(payload) => {
                if payload.max_gas > MAX_GAS_USAGE_PER_TX {
                    return Err(GenerationError::MaxGasReached.into())
                }

                deposits_commitments = Self::build_deposits_commitments::<B::Error>(
                    &payload.deposits,
                    source_keypair.get_public_key(),
                    &None
                )?;
            },
            TransactionTypeBuilder::DeployContract(payload) => {
                if let Some(invoke) = payload.invoke.as_ref() {
                    if invoke.max_gas > MAX_GAS_USAGE_PER_TX {
                        return Err(GenerationError::MaxGasReached.into())
                    }

                    deposits_commitments = Self::build_deposits_commitments::<B::Error>(
                        &invoke.deposits,
                        source_keypair.get_public_key(),
                        &None
                    )?;
                }
            },
            TransactionTypeBuilder::Energy(ref _payload) => {
                // Energy transactions don't need special commitment handling
                // They will be processed in the second match statement
            },
            TransactionTypeBuilder::Burn(_) => {},
            TransactionTypeBuilder::MultiSig(_) => {},
        };

        let reference = state.get_reference();
        let used_assets = self.data.used_assets();

        let mut range_proof_openings: Vec<_> =
            iter::repeat_with(|| PedersenOpening::generate_new().as_scalar())
                .take(used_assets.len())
                .collect();

        let mut range_proof_values: Vec<_> = used_assets
            .iter()
            .map(|asset| {
                let cost = self.get_transaction_cost(fee, &asset);
                let current_balance = state
                .get_account_balance(&asset)
                .map_err(GenerationError::State)?;

                let source_new_balance = current_balance
                    .checked_sub(cost)
                    .ok_or(ProofGenerationError::InsufficientFunds {
                        required: cost,
                        available: current_balance,
                    })?;

                Ok(source_new_balance)
            })
            .collect::<Result<Vec<_>, GenerationError<B::Error>>>()?;

        // Determine fee type: use explicit fee_type if set, otherwise use default logic
        let fee_type = if let Some(ref explicit_fee_type) = self.fee_type {
            explicit_fee_type.clone()
        } else {
            // Default logic: use TOS for all transactions
            FeeType::TOS
        };

        // Prepare the transcript used for proofs
        let mut transcript = Transaction::prepare_transcript(self.version, &self.source, fee, &fee_type, nonce);

        let source_commitments = used_assets
            .into_iter()
            .zip(&range_proof_openings)
            .zip(&range_proof_values)
            .map(|((asset, new_source_opening), &source_new_balance)| {
                let new_source_opening = PedersenOpening::from_scalar(*new_source_opening);

                let source_current_ciphertext = state
                    .get_account_ciphertext(&asset)
                    .map_err(GenerationError::State)?
                    .take_ciphertext()
                    .map_err(|err| GenerationError::Proof(err.into()))?;

                let source_ct_compressed = source_current_ciphertext.compress();

                let commitment =
                    PedersenCommitment::new_with_opening(source_new_balance, &new_source_opening)
                    .compress();

                let new_source_ciphertext =
                    self.get_new_source_ct(source_current_ciphertext, fee, &asset, &transfers_commitments, &deposits_commitments);

                // 1. Make the CommitmentEqProof

                transcript.new_commitment_eq_proof_domain_separator();
                transcript.append_hash(b"new_source_commitment_asset", &asset);
                transcript.append_commitment(b"new_source_commitment", &commitment);

                if self.version >= TxVersion::T0 {
                    transcript.append_ciphertext(b"source_ct", &source_ct_compressed);
                }

                let proof = CommitmentEqProof::new(
                    &source_keypair,
                    &new_source_ciphertext,
                    &new_source_opening,
                    source_new_balance,
                    &mut transcript,
                );

                // Store the new balance in preparation of next transaction
                state
                    .update_account_balance(&asset, source_new_balance, new_source_ciphertext)
                    .map_err(GenerationError::State)?;

                Ok(SourceCommitment::new(commitment, proof, asset.clone()))
            })
            .collect::<Result<Vec<_>, GenerationError<B::Error>>>()?;

        let mut transfers = Vec::new();
        let mut deposits = IndexMap::new();
        match &mut self.data {
            TransactionTypeBuilder::Transfers(_) => {
                range_proof_values.reserve(transfers_commitments.len());
                range_proof_openings.reserve(transfers_commitments.len());

                let mut total_cipher_size = 0;
                transfers = transfers_commitments
                    .into_iter()
                    .map(|transfer| {
                        let commitment = transfer.commitment.compress();
                        let sender_handle = transfer.sender_handle.compress();
                        let receiver_handle = transfer.receiver_handle.compress();
    
                        transcript.transfer_proof_domain_separator();
                        transcript.append_public_key(b"dest_pubkey", transfer.inner.destination.get_public_key());
                        transcript.append_commitment(b"amount_commitment", &commitment);
                        transcript.append_handle(b"amount_sender_handle", &sender_handle);
                        transcript.append_handle(b"amount_receiver_handle", &receiver_handle);
    
                        let source_pubkey = if self.version >= TxVersion::T0 {
                            Some(source_keypair.get_public_key())
                        } else {
                            None
                        };
    
                        let ct_validity_proof = CiphertextValidityProof::new(
                            &transfer.destination,
                            source_pubkey,
                            transfer.inner.amount,
                            &transfer.amount_opening,
                            &mut transcript,
                        );
    
                        range_proof_values.push(transfer.inner.amount);
                        range_proof_openings.push(transfer.amount_opening.as_scalar());
    
                        // Encrypt the extra data if it exists
                        let extra_data = if let Some(extra_data) = transfer.inner.extra_data {
                            let bytes = extra_data.to_bytes();

                            let cipher: UnknownExtraDataFormat = if self.version >= TxVersion::T0 {
                                if transfer.inner.encrypt_extra_data {
                                    ExtraDataType::Private(ExtraData::new(
                                        PlaintextData(bytes),
                                        source_keypair.get_public_key(),
                                        &transfer.destination)
                                    )
                                } else {
                                    ExtraDataType::Public(PlaintextData(bytes))
                                }.into()
                            } else {
                                ExtraData::new(
                                    PlaintextData(bytes),
                                    source_keypair.get_public_key(),
                                    &transfer.destination
                                ).into()
                            };

                            let cipher_size = cipher.size();
                            if cipher_size > EXTRA_DATA_LIMIT_SIZE {
                                return Err(GenerationError::EncryptedExtraDataTooLarge(cipher_size, EXTRA_DATA_LIMIT_SIZE));
                            }
    
                            total_cipher_size += cipher_size;
    
                            Some(cipher)
                        } else {
                            None
                        };
    
                        Ok(TransferPayload::new(
                            transfer.inner.asset,
                            transfer.inner.destination.to_public_key(),
                            extra_data,
                            commitment,
                            sender_handle,
                            receiver_handle,
                            ct_validity_proof,
                        ))
                    })
                    .collect::<Result<Vec<_>, GenerationError<B::Error>>>()?;
    
                if total_cipher_size > EXTRA_DATA_LIMIT_SUM_SIZE {
                    return Err(GenerationError::EncryptedExtraDataTooLarge(total_cipher_size, EXTRA_DATA_LIMIT_SUM_SIZE));
                }
            },
            TransactionTypeBuilder::InvokeContract(payload) => {
                deposits = Self::finalize_deposits_commitments(
                    &mut transcript,
                    &mut range_proof_values,
                    &mut range_proof_openings,
                    &mut payload.deposits,
                    deposits_commitments,
                    source_keypair,
                    &None
                );
            },
            TransactionTypeBuilder::DeployContract(payload) => {
                if let Some(invoke) = payload.invoke.as_mut() {
                    deposits = Self::finalize_deposits_commitments(
                        &mut transcript,
                        &mut range_proof_values,
                        &mut range_proof_openings,
                        &mut invoke.deposits,
                        deposits_commitments,
                        source_keypair,
                        &None
                    );
                }
            }
            _ => {}
        };

        let n_commitments = range_proof_values.len();

        // Create fake commitments to make `m` (party size) of the bulletproof a power of two.
        let n_dud_commitments = n_commitments
            .checked_next_power_of_two()
            .ok_or(ProofGenerationError::Format)?
            - n_commitments;

        range_proof_values.extend(iter::repeat(0u64).take(n_dud_commitments));
        range_proof_openings.extend(iter::repeat(Scalar::ZERO).take(n_dud_commitments));

        let data = match self.data {
            TransactionTypeBuilder::Transfers(_) => TransactionType::Transfers(transfers),
            TransactionTypeBuilder::Burn(ref payload) => {
                // Check if the burn amount is zero
                // Burn of zero are useless and consume fees for nothing
                if payload.amount == 0 {
                    return Err(GenerationError::BurnZero);
                }

                if self.version >= TxVersion::T0 {
                    transcript.burn_proof_domain_separator();
                    transcript.append_hash(b"burn_asset", &payload.asset);
                    transcript.append_u64(b"burn_amount", payload.amount);
                }

                TransactionType::Burn(payload.clone())
            },
            TransactionTypeBuilder::MultiSig(ref payload) => {
                if payload.participants.len() > MAX_MULTISIG_PARTICIPANTS {
                    return Err(GenerationError::MultiSigParticipants);
                }

                if payload.threshold as usize > payload.participants.len() || (payload.threshold == 0 && !payload.participants.is_empty()) {
                    return Err(GenerationError::MultiSigThreshold);
                }

                transcript.multisig_proof_domain_separator();
                transcript.append_u64(b"multisig_threshold", payload.threshold as u64);

                let mut keys = IndexSet::new();
                for addr in &payload.participants {
                    let key = addr.clone().to_public_key();
                    transcript.append_public_key(b"multisig_participant", &key);
                    keys.insert(key);
                }

                // You can't contains yourself in the participants
                if keys.contains(&self.source) {
                    return Err(GenerationError::MultiSigSelfParticipant);
                }

                TransactionType::MultiSig(MultiSigPayload {
                    participants: keys,
                    threshold: payload.threshold,
                })
            },
            TransactionTypeBuilder::InvokeContract(ref payload) => {
                transcript.invoke_contract_proof_domain_separator();
                transcript.append_hash(b"contract_hash", &payload.contract);
                transcript.append_u64(b"max_gas", payload.max_gas);

                for param in payload.parameters.iter() {
                    transcript.append_message(b"contract_param", &param.to_bytes());
                }

                TransactionType::InvokeContract(InvokeContractPayload {
                    contract: payload.contract.clone(),
                    max_gas: payload.max_gas,
                    chunk_id: payload.chunk_id,
                    parameters: payload.parameters.clone(),
                    deposits,
                })
            },
            TransactionTypeBuilder::DeployContract(ref payload) => {
                transcript.deploy_contract_proof_domain_separator();

                let module = Module::from_hex(&payload.module)
                    .map_err(|_| GenerationError::InvalidModule)?;

                if payload.invoke.is_none() != module.get_chunk_id_of_hook(0).is_none() {
                    return Err(GenerationError::InvalidConstructorInvoke);
                }

                TransactionType::DeployContract(DeployContractPayload {
                    module,
                    invoke: payload.invoke.as_ref().map(|invoke| {
                        transcript.invoke_constructor_proof_domain_separator();
                        transcript.append_u64(b"max_gas", invoke.max_gas);

                        InvokeConstructorPayload {
                            max_gas: invoke.max_gas,
                            deposits
                        }
                    }),
                })
            },
            TransactionTypeBuilder::Energy(ref payload) => {
                // Convert EnergyBuilder to EnergyPayload
                let energy_payload = match payload {
                    EnergyBuilder { amount, is_freeze: true, freeze_duration: Some(duration) } => {
                        EnergyPayload::FreezeTos {
                            amount: *amount,
                            duration: duration.clone(),
                        }
                    },
                    EnergyBuilder { amount, is_freeze: false, freeze_duration: None } => {
                        EnergyPayload::UnfreezeTos {
                            amount: *amount,
                        }
                    },
                    _ => {
                        return Err(GenerationError::State("Invalid EnergyBuilder configuration".into()));
                    }
                };

                // Use unified transcript operation for energy transactions
                // This ensures consistency between generation and verification
                Transaction::append_energy_transcript(&mut transcript, &energy_payload);

                TransactionType::Energy(energy_payload)
            }
        };

        // 3. Create the RangeProof

        let (range_proof, _commitments) = RangeProof::prove_multiple(
            &BP_GENS,
            &PC_GENS,
            &mut transcript,
            &range_proof_values,
            &range_proof_openings,
            BULLET_PROOF_SIZE,
        )
        .map_err(ProofGenerationError::from)?;

        // Determine fee type: use explicit fee_type if set, otherwise use default logic
        let fee_type = if let Some(ref explicit_fee_type) = self.fee_type {
            // Only allow Energy for Transfer transactions
            if *explicit_fee_type == super::FeeType::Energy {
                match &self.data {
                    TransactionTypeBuilder::Transfers(_) => {},
                    _ => return Err(GenerationError::State("Energy fee_type only allowed for Transfer transactions".into())),
                }
            }
            explicit_fee_type.clone()
        } else {
            // Default: TOS
            super::FeeType::TOS
        };

        let transaction = UnsignedTransaction::new_with_fee_type(
            self.version,
            self.source,
            data,
            fee,
            fee_type,
            nonce,
            source_commitments,
            reference,
            range_proof,
        );

        Ok(transaction)
    }
}

// Internal struct for build
struct TransferWithCommitment {
    inner: TransferBuilder,
    commitment: PedersenCommitment,
    sender_handle: DecryptHandle,
    receiver_handle: DecryptHandle,
    destination: PublicKey,
    amount_opening: PedersenOpening,
}

impl TransferWithCommitment {
    fn get_ciphertext(&self, role: Role) -> Ciphertext {
        let handle = match role {
            Role::Receiver => self.receiver_handle.clone(),
            Role::Sender => self.sender_handle.clone(),
        };

        Ciphertext::new(self.commitment.clone(), handle)
    }
}

// Internal struct for build
struct DepositWithCommitment {
    amount: u64,
    commitment: PedersenCommitment,
    sender_handle: DecryptHandle,
    receiver_handle: DecryptHandle,
    amount_opening: PedersenOpening,
}

impl DepositWithCommitment {
    fn get_ciphertext(&self, role: Role) -> Ciphertext {
        let handle = match role {
            Role::Receiver => self.receiver_handle.clone(),
            Role::Sender => self.sender_handle.clone(),
        };

        Ciphertext::new(self.commitment.clone(), handle)
    }
}

impl TransactionTypeBuilder {
    // Get the assets used in the transaction
    pub fn used_assets<'a>(&'a self) -> HashSet<&'a Hash> {
        let mut consumed = HashSet::new();

        // Native asset is always used. (fees)
        consumed.insert(&TERMINOS_ASSET);

        match &self {
            TransactionTypeBuilder::Transfers(transfers) => {
                for transfer in transfers {
                    consumed.insert(&transfer.asset);
                }
            }
            TransactionTypeBuilder::Burn(payload) => {
                consumed.insert(&payload.asset);
            },
            TransactionTypeBuilder::InvokeContract(payload) => {
                consumed.extend(payload.deposits.keys());
            },
            _ => {},
        }

        consumed
    }

    // Get the destination keys used in the transaction
    pub fn used_keys<'a>(&'a self) -> HashSet<&'a CompressedPublicKey> {
        let mut used_keys = HashSet::new();

        match &self {
            TransactionTypeBuilder::Transfers(transfers) => {
                for transfer in transfers {
                    used_keys.insert(transfer.destination.get_public_key());
                }
            }
            _ => {},
        }

        used_keys
    }
}

#[cfg(test)]
mod tests {
    use bulletproofs::RangeProof;
    use super::*;

    #[test]
    fn test_estimate_range_proof_size() {
        let proof_hex = b"cc15f1b1e654ffd25bb89f4069303245d3c477ce93abb380eb4941096c06000006141de8f618c3392c5071bc3b76467bea32bc0d8fbf9257a3c44a59b596825f9a09332365fffdb56060d4fdfba8a513cbab3f607c0812aefec7124914cf796caa1a4263cdc0d3488e3e6b5bd04d524667e2b49bb8f55cf418fd8af8cd23ef667bd574ab23bf8c71b1bf9a5f52a2ca5a9320bf43a6be8bb2cc864a6745e6de07931382c2b90873b690e7da04b6fd9ddd3f22c060aed621da691bd54e0b6e9f0b3283b6fc7bcaa4ba06a7f3151a49ba5082462b8ba76b93b2934b6c99fe9e730572e026e9a85930896d0120d06115e60cb68bc6bd18335288ca01f8591924da7e563ac102237e476357b37ecd834715272c5eb705c5bc3799602d922cfa153665565926daf7df42276e834afe1fa444fabf17e7596f09936bcc27f913053fac3906ce8a10dbe1caf1c1e02428d8f2773fc307ae7c7d2fe63102e605c89efa730a4e217dd6b2481f49803efdc44b25d80236e0c10ecab006136ba423ec75bbf7532286a1d063e16e13903104e8274666169288cb9f65a414a04e3dacb7d368931e647a149554f3c78e326e111e5da221cb4e8152d3525f0b32ff2b814b7352647674f1a36e49f8603e3d3996910f52154b871c72138e288b00b471026638646f201c0c0b358872fa6bc81a2ce1c2f068b4513828eda4def4ae1c2e9c02ef58043412dd31411c5cec7acd9bfdcf5f8ead03f13801bc4bc529726e6b25f85b80db23fc8659a09b8c590a51ec015065d437e77d84b0d3c3d529d1c6301441d2dd335042f64b1ced343c32b25416bd5d43e4ff02d4382cc18f1f5cfc0144decc51ac0d9863f1124589ec6f0fe388b464db7db4d5f16ff101da37a3efed71a4d4514915eccc94dc7832bf4c0b52165ac937e5b0dff2d0a2e7b68802a8759e4bae58815f6e2ec7683006561f27f1855ad8840036c580c81ebadf36ddfdf7470996068c05f186a67cefb751e33b5624d577357372486bae3fd509aea9b6d4c72296afdd05";
        let proof = RangeProof::from_bytes(&hex::decode(proof_hex).unwrap()).unwrap();
        let transfers: usize = 1;
        let assets: usize = 1;
        // Range proof size has a fixed size 
        let mut size: usize = RISTRETTO_COMPRESSED_SIZE * 4 + SCALAR_SIZE * 3;
        // U16
        size += 2;
        // inner product scalars
        size += SCALAR_SIZE * 2;
        // G_vec len
        let lg_n = (BULLET_PROOF_SIZE * (transfers + assets)).next_power_of_two().trailing_zeros() as usize;
        size += 2 * RISTRETTO_COMPRESSED_SIZE * lg_n;
        assert!(proof.size() == size);
    }
}