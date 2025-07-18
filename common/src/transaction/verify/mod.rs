mod state;
mod error;
mod contract;

use std::{
    borrow::Cow,
    collections::HashMap,
    iter
};

use bulletproofs::RangeProof;
use curve25519_dalek::{
    ristretto::CompressedRistretto,
    traits::Identity,
    RistrettoPoint,
    Scalar
};
use indexmap::IndexMap;
use log::{debug, trace, error};
use merlin::Transcript;
use terminos_vm::ModuleValidator;
use crate::{
    tokio::block_in_place_safe,
    account::Nonce,
    config::{BURN_PER_CONTRACT, MAX_GAS_USAGE_PER_TX, TERMINOS_ASSET},
    contract::ContractProvider,
    crypto::{
        elgamal::{
            Ciphertext,
            CompressedPublicKey,
            DecompressionError,
            DecryptHandle,
            PedersenCommitment,
            PublicKey
        },
        hash,
        proofs::{
            BatchCollector,
            ProofVerificationError,
            BP_GENS,
            BULLET_PROOF_SIZE,
            PC_GENS
        },
        Hash,
        ProtocolTranscript,
        SIGNATURE_SIZE
    },
    serializer::Serializer,
    transaction::{
        TxVersion,
        EXTRA_DATA_LIMIT_SIZE,
        EXTRA_DATA_LIMIT_SUM_SIZE,
        MAX_DEPOSIT_PER_INVOKE_CALL,
        MAX_MULTISIG_PARTICIPANTS,
        MAX_TRANSFER_COUNT
    },
    utils::calculate_energy_fee,
};
use super::{
    ContractDeposit,
    Role,
    Transaction,
    TransactionType,
    TransferPayload,
    payload::EnergyPayload,
};
use contract::InvokeContract;

pub use state::*;
pub use error::*;

struct DecompressedTransferCt {
    commitment: PedersenCommitment,
    sender_handle: DecryptHandle,
    receiver_handle: DecryptHandle,
}

impl DecompressedTransferCt {
    fn decompress(transfer: &TransferPayload) -> Result<Self, DecompressionError> {
        Ok(Self {
            commitment: transfer.get_commitment().decompress()?,
            sender_handle: transfer.get_sender_handle().decompress()?,
            receiver_handle: transfer.get_receiver_handle().decompress()?,
        })
    }

    fn get_ciphertext(&self, role: Role) -> Ciphertext {
        let handle = match role {
            Role::Receiver => self.receiver_handle.clone(),
            Role::Sender => self.sender_handle.clone(),
        };

        Ciphertext::new(self.commitment.clone(), handle)
    }
}

// Decompressed deposit ciphertext
// Transaction deposits are stored in a compressed format
// We need to decompress them only one time
struct DecompressedDepositCt {
    commitment: PedersenCommitment,
    sender_handle: DecryptHandle,
    receiver_handle: DecryptHandle,
}

impl Transaction {
    // This function will be used to verify the transaction format
    // Modified: All transaction versions now support all operations
    pub fn has_valid_version_format(&self) -> bool {
        // All transaction versions now support all transaction types
        match &self.data {
            TransactionType::Transfers(_)
            | TransactionType::Burn(_)
            | TransactionType::MultiSig(_)
            | TransactionType::InvokeContract(_)
            | TransactionType::DeployContract(_)
            | TransactionType::Energy(_) => true,
        }
    }

    /// Get the new output ciphertext
    /// This is used to substract the amount from the sender's balance
    fn get_sender_output_ct(
        &self,
        asset: &Hash,
        decompressed_transfers: &[DecompressedTransferCt],
        decompressed_deposits: &HashMap<&Hash, DecompressedDepositCt>,
    ) -> Result<Ciphertext, DecompressionError> {
        let mut output = Ciphertext::zero();

        if *asset == TERMINOS_ASSET {
            // Energy can only be used for Transfer transactions
            // For non-transfer transactions, always use TOS fees
            let use_energy_for_fees = self.uses_energy_for_fees();

            if use_energy_for_fees {
                // Use energy for transfer fees - no TOS deduction needed
                // Energy consumption will be handled separately in the apply function
                let energy_cost = self.calculate_energy_cost();
                debug!("Using energy for transfer fees: {} energy", energy_cost);
            } else {
                // Use TOS payment for fees (for all transaction types except energy-enabled transfers)
                // Fees are applied to the native blockchain asset only.
                output += Scalar::from(self.fee);
                debug!("Using TOS for transaction fees: {} TOS", self.fee);
            }
        }

        match &self.data {
            TransactionType::Transfers(transfers) => {
                for (transfer, d) in transfers.iter().zip(decompressed_transfers.iter()) {
                    if asset == transfer.get_asset() {
                        output += d.get_ciphertext(Role::Sender);
                    }
                }
            }
            TransactionType::Burn(payload) => {
                if *asset == payload.asset {
                    output += Scalar::from(payload.amount)
                }
            },
            TransactionType::MultiSig(_) => {},
            TransactionType::InvokeContract(payload) => {
                if *asset == TERMINOS_ASSET {
                    output += Scalar::from(payload.max_gas);
                }

                if let Some(deposit) = payload.deposits.get(asset) {
                    match deposit {
                        ContractDeposit::Public(amount) => {
                            output += Scalar::from(*amount);
                        },
                        ContractDeposit::Private { .. } => {
                            let decompressed = decompressed_deposits.get(asset)
                                .ok_or(DecompressionError)?;

                            output += Ciphertext::new(decompressed.commitment.clone(), decompressed.sender_handle.clone())
                        }
                    }
                }
            },
            TransactionType::DeployContract(payload) => {
                if let Some(invoke) = payload.invoke.as_ref() {
                    if *asset == TERMINOS_ASSET {
                        output += Scalar::from(invoke.max_gas);
                    }

                    if let Some(deposit) = invoke.deposits.get(asset) {
                        match deposit {
                            ContractDeposit::Public(amount) => {
                                output += Scalar::from(*amount);
                            },
                            ContractDeposit::Private { .. } => {
                                let decompressed = decompressed_deposits.get(asset)
                                    .ok_or(DecompressionError)?;

                                output += Ciphertext::new(decompressed.commitment.clone(), decompressed.sender_handle.clone())
                            }
                        }
                    }
                }

                // Burn a full coin for each contract deployed
                if *asset == TERMINOS_ASSET {
                    output += Scalar::from(BURN_PER_CONTRACT);
                }
            },
            TransactionType::Energy(payload) => {
                // Energy operations consume TOS for freeze/unfreeze operations
                // The amount is deducted from TOS balance and converted to energy
                match payload {
                    EnergyPayload::FreezeTos { amount, duration } => {
                        // For freeze operations, deduct the freeze amount from TOS balance
                        if *asset == TERMINOS_ASSET {
                            output += Scalar::from(*amount);
                            let energy_gained = (*amount as f64 * duration.reward_multiplier()) as u64;
                            println!("🔍 FreezeTos operation: deducting {} TOS from balance for asset {}", amount, asset);
                            println!("  Duration: {:?}, Energy gained: {} units", duration, energy_gained);
                        }
                    },
                    EnergyPayload::UnfreezeTos { amount } => {
                        // For unfreeze operations, no TOS deduction (it's returned to balance)
                        // But we still need to account for the energy removal
                        // The amount is already handled in the energy system
                        println!("🔍 UnfreezeTos operation: no TOS deduction for asset {} (amount: {})", asset, amount);
                        println!("  Energy will be removed from energy resource during apply phase");
                    }
                }
            }
        }

        Ok(output)
    }

    /// Get the new output ciphertext for the sender
    pub fn get_expected_sender_outputs<'a>(&'a self) -> Result<Vec<(&'a Hash, Ciphertext)>, DecompressionError> {
        let mut decompressed_transfers = Vec::new();
        let mut decompressed_deposits = HashMap::new();
        match &self.data {
            TransactionType::Transfers(transfers) => {
                decompressed_transfers = transfers
                    .iter()
                    .map(DecompressedTransferCt::decompress)
                    .collect::<Result<_, DecompressionError>>()?;
            },
            TransactionType::InvokeContract(payload) => {
                for (asset, deposit) in &payload.deposits {
                    match deposit {
                        ContractDeposit::Private { commitment, sender_handle, receiver_handle, .. } => {
                            let decompressed = DecompressedDepositCt {
                                commitment: commitment.decompress()?,
                                sender_handle: sender_handle.decompress()?,
                                receiver_handle: receiver_handle.decompress()?,
                            };

                            decompressed_deposits.insert(asset, decompressed);
                        },
                        _ => {}
                    }
                }
            },
            TransactionType::Energy(_) => {},
            _ => {}
        }

        let outputs = self.source_commitments.iter()
            .map(|commitment| {
                let ciphertext = self.get_sender_output_ct(commitment.get_asset(), &decompressed_transfers, &decompressed_deposits)?;
                Ok((commitment.get_asset(), ciphertext))
            })
            .collect::<Result<Vec<_>, DecompressionError>>()?;

        Ok(outputs)
    }

    pub(crate) fn prepare_transcript(
        version: TxVersion,
        source_pubkey: &CompressedPublicKey,
        fee: u64,
        nonce: Nonce,
    ) -> Transcript {
        let mut transcript = Transcript::new(b"transaction-proof");
        transcript.append_u64(b"version", version.into());
        transcript.append_public_key(b"source_pubkey", source_pubkey);
        transcript.append_u64(b"fee", fee);
        transcript.append_u64(b"nonce", nonce);
        transcript
    }

    // Verify that the commitment assets match the assets used in the tx
    fn verify_commitment_assets(&self) -> bool {
        let has_commitment_for_asset = |asset| {
            self.source_commitments
                .iter()
                .any(|c| c.get_asset() == asset)
        };

        // TERMINOS_ASSET is always required for fees
        if !has_commitment_for_asset(&TERMINOS_ASSET) {
            return false;
        }

        // Check for duplicates
        // Don't bother with hashsets or anything, number of transfers should be constrained
        if self
            .source_commitments
            .iter()
            .enumerate()
            .any(|(i, c)| {
                self.source_commitments
                    .iter()
                    .enumerate()
                    .any(|(i2, c2)| i != i2 && c.get_asset() == c2.get_asset())
            })
        {
            return false;
        }

        match &self.data {
            TransactionType::Transfers(transfers) => transfers
                .iter()
                .all(|transfer| has_commitment_for_asset(transfer.get_asset())),
            TransactionType::Burn(payload) => has_commitment_for_asset(&payload.asset),
            TransactionType::MultiSig(_) => true,
            TransactionType::InvokeContract(payload) => payload
                .deposits
                .keys()
                .all(|asset| has_commitment_for_asset(asset)),
            TransactionType::DeployContract(_) => true,
            TransactionType::Energy(_) => true,
        }
    }

    // Verify the format of invoke contract
    fn verify_invoke_contract<'a, E>(
        &self,
        deposits_decompressed: &mut HashMap<&'a Hash, DecompressedDepositCt>,
        deposits: &'a IndexMap<Hash, ContractDeposit>,
        max_gas: u64
    ) -> Result<(), VerificationError<E>> {
        if deposits.len() > MAX_DEPOSIT_PER_INVOKE_CALL {
            return Err(VerificationError::DepositCount);
        }

        if max_gas > MAX_GAS_USAGE_PER_TX {
            return Err(VerificationError::MaxGasReached.into())
        }

        for (asset, deposit) in deposits.iter() {
            match deposit {
                ContractDeposit::Public(amount) => {
                    if *amount == 0 {
                        return Err(VerificationError::InvalidFormat);
                    }
                },
                ContractDeposit::Private { commitment, sender_handle, receiver_handle, .. } => {
                    let decompressed = DecompressedDepositCt {
                        commitment: commitment.decompress()
                            .map_err(ProofVerificationError::from)?,
                        sender_handle: sender_handle.decompress()
                            .map_err(ProofVerificationError::from)?,
                        receiver_handle: receiver_handle.decompress()
                            .map_err(ProofVerificationError::from)?,
                    };

                    deposits_decompressed.insert(asset, decompressed);
                }
            }
        }

        Ok(())
    }

    fn verify_contract_deposits<E>(
        &self,
        transcript: &mut Transcript,
        value_commitments: &mut Vec<(RistrettoPoint, CompressedRistretto)>,
        sigma_batch_collector: &mut BatchCollector,
        source_decompressed: &PublicKey,
        dest_pubkey: &PublicKey,
        deposits_decompressed: &HashMap<&Hash, DecompressedDepositCt>,
        deposits: &IndexMap<Hash, ContractDeposit>,
    ) -> Result<(), VerificationError<E>> {

        for (asset, deposit) in deposits {
            transcript.deposit_proof_domain_separator();
            transcript.append_hash(b"deposit_asset", asset);
            match deposit {
                ContractDeposit::Public(amount) => {
                    transcript.append_u64(b"deposit_plain", *amount);
                },
                ContractDeposit::Private {
                    commitment,
                    sender_handle,
                    receiver_handle,
                    ct_validity_proof
                } => {
                    transcript.append_commitment(b"deposit_commitment", commitment);
                    transcript.append_handle(b"deposit_sender_handle", sender_handle);
                    transcript.append_handle(b"deposit_receiver_handle", receiver_handle);

                    let decompressed = deposits_decompressed.get(asset)
                        .ok_or(VerificationError::DepositNotFound)?;

                    ct_validity_proof.pre_verify(
                        &decompressed.commitment,
                        &dest_pubkey,
                       &source_decompressed,
                        &decompressed.receiver_handle,
                        &decompressed.sender_handle,
                        true,
                        transcript,
                        sigma_batch_collector
                    )?;

                    value_commitments.push((decompressed.commitment.as_point().clone(), commitment.as_point().clone()));
                }
            }
        }

        Ok(())
    }

    // internal, does not verify the range proof
    // returns (transcript, commitments for range proof)
    async fn pre_verify<'a, E, B: BlockchainVerificationState<'a, E>>(
        &'a self,
        tx_hash: &'a Hash,
        state: &mut B,
        sigma_batch_collector: &mut BatchCollector,
    ) -> Result<(Transcript, Vec<(RistrettoPoint, CompressedRistretto)>), VerificationError<E>>
    {
        trace!("Pre-verifying transaction");
        if !self.has_valid_version_format() {
            return Err(VerificationError::InvalidFormat);
        }

        // Validate that energy fees are only used for Transfer transactions
        if self.uses_energy_fees() && !matches!(self.get_data(), TransactionType::Transfers(_)) {
            debug!("Energy fees can only be used for Transfer transactions");
            return Err(VerificationError::EnergyFeesNotAllowedForNonTransfer);
        }

        trace!("Pre-verifying transaction on state");
        state.pre_verify_tx(&self).await
            .map_err(VerificationError::State)?;

        // First, check the nonce
        let account_nonce = state.get_account_nonce(&self.source).await
            .map_err(VerificationError::State)?;

        if account_nonce != self.nonce {
            return Err(VerificationError::InvalidNonce(account_nonce, self.nonce));
        }

        // Nonce is valid, update it for next transactions if any
        state
            .update_account_nonce(&self.source, self.nonce + 1).await
            .map_err(VerificationError::State)?;

        if !self.verify_commitment_assets() {
            debug!("Invalid commitment assets");
            return Err(VerificationError::Commitments);
        }

        let mut transfers_decompressed: Vec<_> = Vec::new();
        let mut deposits_decompressed: HashMap<_, _> = HashMap::new();
        match &self.data {
            TransactionType::Transfers(transfers) => {
                if transfers.len() > MAX_TRANSFER_COUNT || transfers.is_empty() {
                    debug!("incorrect transfers size: {}", transfers.len());
                    return Err(VerificationError::TransferCount);
                }

                let mut extra_data_size = 0;
                // Prevent sending to ourself
                for transfer in transfers.iter() {
                    if *transfer.get_destination() == self.source {
                        debug!("sender cannot be the receiver in the same TX");
                        return Err(VerificationError::SenderIsReceiver);
                    }

                    if let Some(extra_data) = transfer.get_extra_data() {
                        let size = extra_data.size();
                        if size > EXTRA_DATA_LIMIT_SIZE {
                            return Err(VerificationError::TransferExtraDataSize);
                        }
                        extra_data_size += size;
                    }

                    let decompressed = DecompressedTransferCt::decompress(transfer)
                        .map_err(ProofVerificationError::from)?;

                    transfers_decompressed.push(decompressed);
                }
    
                // Check the sum of extra data size
                if extra_data_size > EXTRA_DATA_LIMIT_SUM_SIZE {
                    return Err(VerificationError::TransactionExtraDataSize);
                }
            },
            TransactionType::Burn(payload) => {
                let fee = self.fee;
                let amount = payload.amount;

                if amount == 0 {
                    return Err(VerificationError::InvalidFormat);
                }

                let total = fee.checked_add(amount)
                    .ok_or(VerificationError::InvalidFormat)?;

                if total < fee || total < amount {
                    return Err(VerificationError::InvalidFormat);
                }
            },
            TransactionType::MultiSig(payload) => {
                if payload.participants.len() > MAX_MULTISIG_PARTICIPANTS {
                    return Err(VerificationError::MultiSigParticipants);
                }

                // Threshold should be less than or equal to the number of participants
                if payload.threshold as usize > payload.participants.len() {
                    return Err(VerificationError::MultiSigThreshold);
                }

                // If the threshold is set to 0, while we have participants, its invalid
                // Threshold should be always > 0
                if payload.threshold == 0 && !payload.participants.is_empty() {
                    return Err(VerificationError::MultiSigThreshold);
                }

                // You can't contains yourself in the participants
                if payload.participants.contains(self.get_source()) {
                    return Err(VerificationError::MultiSigParticipants);
                }

                let is_reset = payload.threshold == 0 && payload.participants.is_empty();
                // If the multisig is reset, we need to check if it was already configured
                if is_reset && state.get_multisig_state(&self.source).await.map_err(VerificationError::State)?.is_none() {
                    return Err(VerificationError::MultiSigNotConfigured);
                }
            },
            TransactionType::InvokeContract(payload) => {
                self.verify_invoke_contract(
                    &mut deposits_decompressed,
                    &payload.deposits,
                    payload.max_gas
                )?;

                // We need to load the contract module if not already in cache
                if !self.is_contract_available(state, &payload.contract).await? {
                    return Err(VerificationError::ContractNotFound);
                }

                let (module, environment) = state.get_contract_module_with_environment(&payload.contract).await
                    .map_err(VerificationError::State)?;

                if !module.is_entry_chunk(payload.chunk_id as usize) {
                    return Err(VerificationError::InvalidInvokeContract);
                }

                let validator = ModuleValidator::new(module, environment);
                for constant in payload.parameters.iter() {
                    validator.verify_constant(&constant)
                        .map_err(|err| VerificationError::ModuleError(format!("{:#}", err)))?;
                }
            },
            TransactionType::DeployContract(payload) => {
                if let Some(invoke) = payload.invoke.as_ref() {
                    self.verify_invoke_contract(
                        &mut deposits_decompressed,
                        &invoke.deposits,
                        invoke.max_gas
                    )?;
                }

                let environment = state.get_environment().await
                    .map_err(VerificationError::State)?;

                let validator = ModuleValidator::new(&payload.module, environment);
                validator.verify()
                    .map_err(|err| VerificationError::ModuleError(format!("{:#}", err)))?;
            },
            TransactionType::Energy(_) => {
                // Energy operations are validated by the energy module
                // No additional verification needed here
            }
        };

        let new_source_commitments_decompressed = self
            .source_commitments
            .iter()
            .map(|commitment| commitment.get_commitment().decompress())
            .collect::<Result<Vec<_>, DecompressionError>>()
            .map_err(ProofVerificationError::from)?;

        let source_decompressed = self
            .source
            .decompress()
            .map_err(|err| VerificationError::Proof(err.into()))?;

        let mut transcript = Self::prepare_transcript(self.version, &self.source, self.fee, self.nonce);

        // 0.a Verify Signature
        let bytes = self.to_bytes();
        if !self.signature.verify(&bytes[..bytes.len() - SIGNATURE_SIZE], &source_decompressed) {
            debug!("transaction signature is invalid");
            return Err(VerificationError::InvalidSignature);
        }

        // 0.b Verify multisig
        if let Some(config) = state.get_multisig_state(&self.source).await.map_err(VerificationError::State)? {
            let Some(multisig) = self.get_multisig() else {
                return Err(VerificationError::MultiSigNotFound);
            };

            if (config.threshold as usize) != multisig.len() || multisig.len() > MAX_MULTISIG_PARTICIPANTS {
                return Err(VerificationError::MultiSigParticipants);
            }

            // Multisig are based on the Tx data, without the final signature
            // We need to remove the final signature and the multisig from the bytes
            // Each SigId is composed of a u8 and a signature (64 bytes + 1 byte)
            // We have overhead of 1 byte for the optional bool, and 1 byte for the count in u8
            // We also need to get rid of the final signature (64 bytes)
            let size = 1 + 1 + SIGNATURE_SIZE + multisig.len() * (SIGNATURE_SIZE + 1);
            if  size >= bytes.len() {
                return Err(VerificationError::InvalidFormat);
            }

            let hash = hash(&bytes[..bytes.len() - size]);
            for sig in multisig.get_signatures() {
                // A participant can't sign more than once because of the IndexSet (SignatureId impl Hash on id)
                let index = sig.id as usize;
                let Some(key) = config.participants.get_index(index) else {
                    return Err(VerificationError::MultiSigParticipants);
                };

                let decompressed = key.decompress().map_err(ProofVerificationError::from)?;
                if !sig.signature.verify(hash.as_bytes(), &decompressed) {
                    return Err(VerificationError::InvalidSignature);
                }
            }
        } else if self.get_multisig().is_some() {
            return Err(VerificationError::MultiSigNotConfigured);
        }

        // 1. Verify CommitmentEqProofs
        trace!("verifying commitments eq proofs");

        for (commitment, new_source_commitment) in self
            .source_commitments
            .iter()
            .zip(&new_source_commitments_decompressed)
        {
            debug!("Verifying commitment for asset: {}", commitment.get_asset());
            
            // Ciphertext containing all the funds spent for this commitment
            let output = match self.get_sender_output_ct(commitment.get_asset(), &transfers_decompressed, &deposits_decompressed) {
                Ok(output) => {
                    debug!("Successfully computed sender output for asset {}", commitment.get_asset());
                    output
                },
                Err(e) => {
                    error!("Failed to compute sender output for asset {}: {:?}", commitment.get_asset(), e);
                    return Err(VerificationError::Proof(ProofVerificationError::from(e)));
                }
            };

            // Retrieve the balance of the sender
            let source_verification_ciphertext = match state
                .get_sender_balance(&self.source, commitment.get_asset(), &self.reference).await
            {
                Ok(balance) => {
                    debug!("Retrieved sender balance for asset {}", commitment.get_asset());
                    balance
                },
                Err(e) => {
                    error!("Failed to retrieve sender balance for asset {}", commitment.get_asset());
                    return Err(VerificationError::State(e));
                }
            };

            let source_ct_compressed = source_verification_ciphertext.compress();

            // Compute the new final balance for account
            *source_verification_ciphertext -= &output;
            transcript.new_commitment_eq_proof_domain_separator();
            transcript.append_hash(b"new_source_commitment_asset", commitment.get_asset());
            transcript
                .append_commitment(b"new_source_commitment", commitment.get_commitment());

            if self.version >= TxVersion::V0 {
                transcript.append_ciphertext(b"source_ct", &source_ct_compressed);
            }

            // Verify commitment equality proof with detailed error information
            match commitment.get_proof().pre_verify(
                &source_decompressed,
                &source_verification_ciphertext,
                &new_source_commitment,
                &mut transcript,
                sigma_batch_collector,
            ) {
                Ok(()) => {
                    debug!("Commitment equality proof verified successfully for asset {}", commitment.get_asset());
                },
                Err(e) => {
                    error!("Commitment equality proof verification failed for asset {}: {:?}", commitment.get_asset(), e);
                    error!("Transaction details: fee={}, nonce={}, data={:?}", self.fee, self.nonce, self.data);
                    return Err(VerificationError::Proof(e));
                }
            }

            // Update source balance
            match state
                .add_sender_output(
                    &self.source,
                    commitment.get_asset(),
                    output,
                ).await
            {
                Ok(()) => {
                    debug!("Successfully updated sender output for asset {}", commitment.get_asset());
                },
                Err(e) => {
                    error!("Failed to update sender output for asset {}", commitment.get_asset());
                    return Err(VerificationError::State(e));
                }
            }
        }

        // 2. Verify every CtValidityProof
        trace!("verifying transfers ciphertext validity proofs");

        // Prepare the new source commitments at same time
        // Count the number of commitments
        let mut value_commitments: Vec<(RistrettoPoint, CompressedRistretto)> = Vec::new();

        match &self.data {
            TransactionType::Transfers(transfers) => {
                // Prepare the new commitments
                for (transfer, decompressed) in transfers.iter().zip(&transfers_decompressed) {
                    let receiver = transfer
                        .get_destination()
                        .decompress()
                        .map_err(ProofVerificationError::from)?;
    
                    // Update receiver balance
    
                    let current_balance = state
                        .get_receiver_balance(
                            Cow::Borrowed(transfer.get_destination()),
                            Cow::Borrowed(transfer.get_asset())
                        ).await
                        .map_err(VerificationError::State)?;

                    let receiver_ct = decompressed.get_ciphertext(Role::Receiver);
                    *current_balance += receiver_ct;

                    // Validity proof

                    transcript.transfer_proof_domain_separator();
                    transcript.append_public_key(b"dest_pubkey", transfer.get_destination());
                    transcript.append_commitment(b"amount_commitment", transfer.get_commitment());
                    transcript.append_handle(b"amount_sender_handle", transfer.get_sender_handle());
                    transcript
                        .append_handle(b"amount_receiver_handle", transfer.get_receiver_handle());

                    transfer.get_proof().pre_verify(
                        &decompressed.commitment,
                        &receiver,
                        &source_decompressed,
                        &decompressed.receiver_handle,
                        &decompressed.sender_handle,
                        self.version >= TxVersion::V0,
                        &mut transcript,
                        sigma_batch_collector,
                    )?;

                    // Add the commitment to the list
                    value_commitments.push((decompressed.commitment.as_point().clone(), transfer.get_commitment().as_point().clone()));
                }
                
                // Add Energy fee transcript operations for transfer transactions using Energy fees
                // This ensures consistency between build and verification phases
                if self.uses_energy_for_fees() {
                    // Use the same calculation method as build phase
                    // For verification, we need to estimate the size based on the actual transaction
                    let tx_size = self.size();
                    
                    // For verification, we need to use the same new_addresses calculation as build phase
                    // Since we don't have access to account registration status in verification,
                    // we assume the destination account doesn't exist (new_addresses = 1) to match build phase
                    let new_addresses = 1; // Assume destination account doesn't exist
                    
                    let energy_cost = calculate_energy_fee(
                        tx_size,
                        transfers.len(),
                        new_addresses
                    );
                    
                    println!("🔍 Transfer with Energy fees transcript operation (verification phase):");
                    println!("  Energy cost: {} units", energy_cost);
                    println!("  Fee: {}, Nonce: {}", self.fee, self.nonce);
                    println!("  Transaction size: {} bytes, Transfer count: {}", tx_size, transfers.len());
                    
                    transcript.append_u64(b"transfer_energy_fee", energy_cost);
                    transcript.append_u64(b"transfer_uses_energy", 1);
                    
                    println!("  Transfer Energy fee transcript operation completed (verification phase)");
                    debug!("Transfer with Energy fees (verification) - energy_cost: {}, fee: {}, nonce: {}", 
                           energy_cost, self.fee, self.nonce);
                } else {
                    // For TOS fees, add TOS fee information to transcript
                    transcript.append_u64(b"transfer_tos_fee", self.fee);
                    transcript.append_u64(b"transfer_uses_energy", 0);
                    
                    debug!("Transfer with TOS fees (verification) - fee: {}, nonce: {}", self.fee, self.nonce);
                }
            },
            TransactionType::Burn(payload) => {
                if self.get_version() >= TxVersion::V0 {
                    transcript.burn_proof_domain_separator();
                    transcript.append_hash(b"burn_asset", &payload.asset);
                    transcript.append_u64(b"burn_amount", payload.amount);
                }
            },
            TransactionType::MultiSig(payload) => {
                transcript.multisig_proof_domain_separator();
                transcript.append_u64(b"multisig_threshold", payload.threshold as u64);
                for key in &payload.participants {
                    transcript.append_public_key(b"multisig_participant", key);
                }

                // Setup the multisig
                state.set_multisig_state(&self.source, payload).await
                    .map_err(VerificationError::State)?;
            },
            TransactionType::InvokeContract(payload) => {                
                let dest_pubkey = PublicKey::from_hash(&payload.contract);
                self.verify_contract_deposits(
                    &mut transcript,
                    &mut value_commitments,
                    sigma_batch_collector,
                    &source_decompressed,
                    &dest_pubkey,
                    &deposits_decompressed,
                    &payload.deposits,
                )?;

                transcript.invoke_contract_proof_domain_separator();
                transcript.append_hash(b"contract_hash", &payload.contract);
                transcript.append_u64(b"max_gas", payload.max_gas);

                for param in payload.parameters.iter() {
                    transcript.append_message(b"contract_param", &param.to_bytes());
                }
            },
            TransactionType::DeployContract(payload) => {
                transcript.deploy_contract_proof_domain_separator();

                // Verify that if we have a constructor, we must have an invoke, and vice-versa
                if payload.invoke.is_none() != payload.module.get_chunk_id_of_hook(0).is_none() {
                    return Err(VerificationError::InvalidFormat);
                }

                if let Some(invoke) = payload.invoke.as_ref() {
                    let dest_pubkey = PublicKey::from_hash(&tx_hash);
                    self.verify_contract_deposits(
                        &mut transcript,
                        &mut value_commitments,
                        sigma_batch_collector,
                        &source_decompressed,
                        &dest_pubkey,
                        &deposits_decompressed,
                        &invoke.deposits,
                    )?;

                    transcript.invoke_constructor_proof_domain_separator();
                    transcript.append_u64(b"max_gas", invoke.max_gas);
                }

                state.set_contract_module(tx_hash, &payload.module).await
                    .map_err(VerificationError::State)?;
            },
            TransactionType::Energy(_) => {
                // Energy operations are validated by the energy module
                // No additional verification needed here
            }
        }

        // Finalize the new source commitments

        // Create fake commitments to make `m` (party size) of the bulletproof a power of two.
        let n_commitments = self.source_commitments.len() + value_commitments.len();
        let n_dud_commitments = n_commitments
            .checked_next_power_of_two()
            .ok_or(ProofVerificationError::Format)?
            - n_commitments;

        let final_commitments = self
            .source_commitments
            .iter()
            .zip(new_source_commitments_decompressed)
            .map(|(commitment, new_source_commitment)| {
                (
                    new_source_commitment.to_point(),
                    commitment.get_commitment().as_point().clone(),
                )
            })
            .chain(value_commitments.into_iter())
            .chain(
                iter::repeat((RistrettoPoint::identity(), CompressedRistretto::identity()))
                    .take(n_dud_commitments),
            )
            .collect();

        // 3. Verify the aggregated RangeProof
        trace!("verifying range proof");

        // range proof will be verified in batch by caller

        Ok((transcript, final_commitments))
    }

    pub async fn verify_batch<'a, T: AsRef<Transaction>, H: AsRef<Hash>, E, B: BlockchainVerificationState<'a, E>>(
        txs: &'a [(T, H)],
        state: &mut B,
    ) -> Result<(), VerificationError<E>> {
        trace!("Verifying batch of {} transactions", txs.len());
        let mut sigma_batch_collector = BatchCollector::default();
        let mut prepared = Vec::with_capacity(txs.len());
        for (tx, hash) in txs {
            let (transcript, commitments) = tx.as_ref()
                .pre_verify(hash.as_ref(), state, &mut sigma_batch_collector).await?;
            prepared.push((transcript, commitments));
        }

        block_in_place_safe(|| {
            sigma_batch_collector
                .verify()
                .map_err(|_| ProofVerificationError::GenericProof)?;
    
            RangeProof::verify_batch(
                txs.iter()
                    .zip(&mut prepared)
                    .map(|((tx, _), (transcript, commitments))| {
                        tx.as_ref()
                            .range_proof
                            .verification_view(
                                transcript,
                                commitments,
                                BULLET_PROOF_SIZE
                            )
                    }),
                &BP_GENS,
                &PC_GENS,
            )
            .map_err(ProofVerificationError::from)
        })?;

        Ok(())
    }

    /// Verify one transaction. Use `verify_batch` to verify a batch of transactions.
    pub async fn verify<'a, E, B: BlockchainVerificationState<'a, E>>(
        &'a self,
        tx_hash: &'a Hash,
        state: &mut B,
    ) -> Result<(), VerificationError<E>> {
        let mut sigma_batch_collector = BatchCollector::default();
        let (mut transcript, commitments) = self.pre_verify(tx_hash, state, &mut sigma_batch_collector).await?;

        block_in_place_safe(|| {
            trace!("Verifying sigma proofs for transaction {}", tx_hash);
            
            // Verify sigma proofs with detailed error information
            match sigma_batch_collector.verify() {
                Ok(()) => {
                    debug!("Sigma proofs verification successful for transaction {}", tx_hash);
                },
                Err(_) => {
                    error!("Sigma proofs verification failed for transaction {}", tx_hash);
                    error!("Transaction details: fee={}, nonce={}, data={:?}", 
                           self.fee, self.nonce, self.data);
                    error!("Source commitments count: {}", self.source_commitments.len());
                    return Err(ProofVerificationError::GenericProof);
                }
            }

            trace!("Verifying range proof for transaction {}", tx_hash);
            
            // Add detailed debugging information for range proof verification
            println!("🔍 Range proof verification details:");
            println!("  Transaction type: {:?}", self.data);
            println!("  Transaction hash: {}", tx_hash);
            println!("  Fee: {}, Nonce: {}", self.fee, self.nonce);
            println!("  Source commitments count: {}", self.source_commitments.len());
            println!("  Total commitments count: {}", commitments.len());
            println!("  Range proof size: {} bytes", self.range_proof.size());
            println!("  Bulletproof size: {}", BULLET_PROOF_SIZE);
            
            // Print source commitment details
            for (i, commitment) in self.source_commitments.iter().enumerate() {
                println!("  Source commitment {}: asset={}, commitment={:?}", 
                         i, commitment.get_asset(), commitment.get_commitment());
            }
            
            // Print all commitments for range proof verification
            println!("  Commitments for range proof verification:");
            for (i, (new_commitment, old_commitment)) in commitments.iter().enumerate() {
                println!("    Commitment {}: new={:?}, old={:?}", i, new_commitment, old_commitment);
            }
            
            debug!("Range proof verification details:");
            debug!("  Transaction type: {:?}", self.data);
            debug!("  Source commitments count: {}", self.source_commitments.len());
            debug!("  Total commitments count: {}", commitments.len());
            debug!("  Range proof size: {} bytes", self.range_proof.size());
            debug!("  Bulletproof size: {}", BULLET_PROOF_SIZE);
            
            // Verify range proof with detailed error information
            println!("🔍 Starting range proof verification...");
            match RangeProof::verify_multiple(
                &self.range_proof,
                &BP_GENS,
                &PC_GENS,
                &mut transcript,
                &commitments,
                BULLET_PROOF_SIZE,
            ) {
                Ok(()) => {
                    println!("✅ Range proof verification successful for transaction {}", tx_hash);
                    debug!("Range proof verification successful for transaction {}", tx_hash);
                },
                Err(e) => {
                    println!("❌ Range proof verification failed for transaction {}: {:?}", tx_hash, e);
                    println!("❌ Transaction details: fee={}, nonce={}, data={:?}", 
                             self.fee, self.nonce, self.data);
                    println!("❌ Source commitments count: {}", self.source_commitments.len());
                    println!("❌ Total commitments count: {}", commitments.len());
                    println!("❌ Range proof size: {} bytes", self.range_proof.size());
                    println!("❌ Bulletproof size: {}", BULLET_PROOF_SIZE);
                    
                    // Print detailed commitment information for debugging
                    println!("❌ Detailed commitment information:");
                    for (i, (new_commitment, old_commitment)) in commitments.iter().enumerate() {
                        println!("    Commitment {}: new={:?}, old={:?}", i, new_commitment, old_commitment);
                    }
                    
                    // Print transcript state information
                    println!("❌ Transcript state before range proof verification:");
                    let mut challenge = [0u8; 32];
                    transcript.challenge_bytes(b"debug_challenge", &mut challenge);
                    println!("    Transcript challenge: {:?}", challenge);
                    
                    error!("Range proof verification failed for transaction {}: {:?}", tx_hash, e);
                    error!("Transaction details: fee={}, nonce={}, data={:?}", 
                           self.fee, self.nonce, self.data);
                    error!("Source commitments count: {}", self.source_commitments.len());
                    error!("Total commitments count: {}", commitments.len());
                    error!("Range proof size: {} bytes", self.range_proof.size());
                    error!("Bulletproof size: {}", BULLET_PROOF_SIZE);
                    
                    // Log commitment details for debugging
                    for (i, (new_commitment, old_commitment)) in commitments.iter().enumerate() {
                        debug!("  Commitment {}: new={:?}, old={:?}", i, new_commitment, old_commitment);
                    }
                    
                    return Err(ProofVerificationError::from(e));
                }
            }
            
            Ok(())
        })?;
    
        Ok(())
    }

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

    /// Check if this transaction uses energy for fees
    /// Energy can only be used for Transfer transactions to provide free TOS and other token transfers
    pub fn uses_energy_for_fees(&self) -> bool {
        // Energy can only be used for Transfer transactions
        // This provides users with the opportunity to stake TOS for free transfers
        self.uses_energy_fees() && matches!(self.get_data(), TransactionType::Transfers(_))
    }

    /// Get the fee amount (either TOS fee or energy cost)
    pub fn get_fee_amount(&self) -> (u64, bool) {
        if self.uses_energy_for_fees() {
            (self.calculate_energy_cost(), true) // (energy_cost, is_energy)
        } else {
            (self.fee, false) // (tos_fee, is_energy)
        }
    }

    // Apply the transaction to the state
    // Arc is required around Self to be shared easily into the VM if needed
    async fn apply<'a, P: ContractProvider, E: From<String>, B: BlockchainApplyState<'a, P, E>>(
        &'a self,
        tx_hash: &'a Hash,
        state: &mut B,
        decompressed_deposits: &HashMap<&Hash, DecompressedDepositCt>,
    ) -> Result<(), VerificationError<E>> {
        trace!("Applying transaction data");
        
        // Validate that energy fees are only used for Transfer transactions
        if self.uses_energy_fees() && !matches!(self.get_data(), TransactionType::Transfers(_)) {
            debug!("Energy fees can only be used for Transfer transactions");
            return Err(VerificationError::EnergyFeesNotAllowedForNonTransfer);
        }
        
        // Update nonce
        state.update_account_nonce(self.get_source(), self.nonce + 1).await
            .map_err(VerificationError::State)?;

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

        // Apply receiver balances
        match &self.data {
            TransactionType::Transfers(transfers) => {
                for transfer in transfers {
                    // Update receiver balance
                    let current_balance = state
                        .get_receiver_balance(
                            Cow::Borrowed(transfer.get_destination()),
                            Cow::Borrowed(transfer.get_asset()),
                        ).await
                        .map_err(VerificationError::State)?;
    
                    let receiver_ct = transfer
                        .get_ciphertext(Role::Receiver)
                        .decompress()
                        .map_err(ProofVerificationError::from)?;
    
                    *current_balance += receiver_ct;
                }
            },
            TransactionType::Burn(payload) => {
                if payload.asset == TERMINOS_ASSET {
                    state.add_burned_coins(payload.amount).await
                        .map_err(VerificationError::State)?;
                }
            },
            TransactionType::MultiSig(payload) => {
                state.set_multisig_state(&self.source, payload).await.map_err(VerificationError::State)?;
            },
            TransactionType::InvokeContract(payload) => {
                let is_success = self.invoke_contract(
                    tx_hash,
                    state,
                    decompressed_deposits,
                    &payload.contract,
                    &payload.deposits,
                    payload.parameters.iter().cloned(),
                    payload.max_gas,
                    InvokeContract::Entry(payload.chunk_id)
                ).await?;

                if !is_success {
                    debug!("Contract invocation for {} failed", tx_hash);
                }
            },
            TransactionType::DeployContract(payload) => {
                state.set_contract_module(tx_hash, &payload.module).await
                    .map_err(VerificationError::State)?;

                if let Some(invoke) = payload.invoke.as_ref() {
                    let is_success = self.invoke_contract(
                        tx_hash,
                        state,
                        decompressed_deposits,
                        tx_hash,
                        &invoke.deposits,
                        iter::empty(),
                        invoke.max_gas,
                        InvokeContract::Hook(0)
                    ).await?;

                    // if it has failed, we don't want to deploy the contract
                    // TODO: we must handle this carefully
                    if !is_success {
                        debug!("Contract deploy for {} failed", tx_hash);
                        state.remove_contract_module(tx_hash).await
                            .map_err(VerificationError::State)?;
                    }
                }
            },
            TransactionType::Energy(payload) => {
                // Handle energy operations (freeze/unfreeze TOS)
                match payload {
                    EnergyPayload::FreezeTos { amount, duration } => {
                        // Get current energy resource
                        let mut energy_resource = state.get_energy_resource(&self.source).await
                            .map_err(VerificationError::State)?;
                        
                        // Get current topoheight for freeze calculation
                        let current_topoheight = state.get_topo_height();
                        
                        // Freeze TOS and get energy
                        let energy_gained = energy_resource.freeze_tos_for_energy(*amount, duration.clone(), current_topoheight);
                        
                        // Update energy resource in state
                        state.update_energy_resource(&self.source, energy_resource).await
                            .map_err(VerificationError::State)?;
                        
                        debug!("Froze {} TOS for {} days, gained {} energy", amount, duration.duration_in_blocks() / (24 * 60 * 60), energy_gained);
                    },
                    EnergyPayload::UnfreezeTos { amount } => {
                        // Get current energy resource
                        let mut energy_resource = state.get_energy_resource(&self.source).await
                            .map_err(VerificationError::State)?;
                        
                        // Get current topoheight for unfreeze validation
                        let current_topoheight = state.get_topo_height();
                        
                        println!("🔍 UnfreezeTos apply operation:");
                        println!("  Amount to unfreeze: {} TOS", amount);
                        println!("  Current topoheight: {}", current_topoheight);
                        println!("  Current frozen TOS: {} TOS", energy_resource.frozen_tos);
                        println!("  Current total energy: {} units", energy_resource.total_energy);
                        
                        // Unfreeze TOS
                        let energy_removed = energy_resource.unfreeze_tos(*amount, current_topoheight)
                            .map_err(|e| {
                                println!("❌ UnfreezeTos failed: {}", e);
                                VerificationError::State(e.into())
                            })?;
                        
                        // Update energy resource in state
                        state.update_energy_resource(&self.source, energy_resource).await
                            .map_err(VerificationError::State)?;
                        
                        println!("✅ UnfreezeTos successful:");
                        println!("  Unfroze: {} TOS", amount);
                        println!("  Energy removed: {} units", energy_removed);
                        
                        debug!("Unfroze {} TOS, removed {} energy", amount, energy_removed);
                    }
                }
            }
        }

        Ok(())
    }

    /// Assume the tx is valid, apply it to `state`. May panic if a ciphertext is ill-formed.
    pub async fn apply_without_verify<'a, P: ContractProvider, E: From<String>, B: BlockchainApplyState<'a, P, E>>(
        &'a self,
        tx_hash: &'a Hash,
        state: &mut B,
    ) -> Result<(), VerificationError<E>> {
        // Validate that energy fees are only used for Transfer transactions
        if self.uses_energy_fees() && !matches!(self.get_data(), TransactionType::Transfers(_)) {
            debug!("Energy fees can only be used for Transfer transactions");
            return Err(VerificationError::EnergyFeesNotAllowedForNonTransfer);
        }
        
        let mut transfers_decompressed = Vec::new();
        let mut deposits_decompressed = HashMap::new();
        match &self.data {
            TransactionType::Transfers(transfers) => {
                transfers_decompressed = transfers
                    .iter()
                    .map(DecompressedTransferCt::decompress)
                    .collect::<Result<_, DecompressionError>>()
                    .map_err(ProofVerificationError::from)?
            },
            TransactionType::InvokeContract(payload) => {
                for (asset, deposit) in &payload.deposits {
                    match deposit {
                        ContractDeposit::Private { commitment, sender_handle, receiver_handle, .. } => {
                            let decompressed = DecompressedDepositCt {
                                commitment: commitment.decompress()
                                    .map_err(ProofVerificationError::from)?,
                                sender_handle: sender_handle.decompress()
                                    .map_err(ProofVerificationError::from)?,
                                receiver_handle: receiver_handle.decompress()
                                    .map_err(ProofVerificationError::from)?,
                            };

                            deposits_decompressed.insert(asset, decompressed);
                        },
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        // We don't verify any proof, we just apply the transaction
        for commitment in &self.source_commitments {
            let asset = commitment.get_asset();
            let current_source_balance = state
                .get_sender_balance(
                    &self.source,
                    asset,
                    &self.reference,
                ).await.map_err(VerificationError::State)?;

            let output = self.get_sender_output_ct(asset, &transfers_decompressed, &deposits_decompressed)
                .map_err(ProofVerificationError::from)?;

            // Compute the new final balance for account
            *current_source_balance -= &output;

            // Update source balance
            state.add_sender_output(
                &self.source,
                asset,
                output,
            ).await.map_err(VerificationError::State)?;
        }

        self.apply(tx_hash, state, &deposits_decompressed).await
    }

    /// Verify only that the final sender balance is the expected one for each commitment
    /// Then apply ciphertexts to the state
    /// Checks done are: commitment eq proofs only
    pub async fn apply_with_partial_verify<'a, P: ContractProvider, E: From<String>, B: BlockchainApplyState<'a, P, E>>(
        &'a self,
        tx_hash: &'a Hash,
        state: &mut B
    ) -> Result<(), VerificationError<E>> {
        trace!("apply with partial verify");
        
        // Validate that energy fees are only used for Transfer transactions
        if self.uses_energy_fees() && !matches!(self.get_data(), TransactionType::Transfers(_)) {
            debug!("Energy fees can only be used for Transfer transactions");
            return Err(VerificationError::EnergyFeesNotAllowedForNonTransfer);
        }
        
        let mut sigma_batch_collector = BatchCollector::default();

        let mut transfers_decompressed = Vec::new();
        let mut deposits_decompressed = HashMap::new();
        match &self.data {
            TransactionType::Transfers(transfers) => {
                transfers_decompressed = transfers
                    .iter()
                    .map(DecompressedTransferCt::decompress)
                    .collect::<Result<_, DecompressionError>>()
                    .map_err(ProofVerificationError::from)?
            },
            TransactionType::InvokeContract(payload) => {
                for (asset, deposit) in &payload.deposits {
                    match deposit {
                        ContractDeposit::Private { commitment, sender_handle, receiver_handle, .. } => {
                            let decompressed = DecompressedDepositCt {
                                commitment: commitment.decompress()
                                    .map_err(ProofVerificationError::from)?,
                                sender_handle: sender_handle.decompress()
                                    .map_err(ProofVerificationError::from)?,
                                receiver_handle: receiver_handle.decompress()
                                    .map_err(ProofVerificationError::from)?,
                            };

                            deposits_decompressed.insert(asset, decompressed);
                        },
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        let new_source_commitments_decompressed = self
            .source_commitments
            .iter()
            .map(|commitment| commitment.get_commitment().decompress())
            .collect::<Result<Vec<_>, DecompressionError>>()
            .map_err(ProofVerificationError::from)?;

        let owner = self
            .source
            .decompress()
            .map_err(|err| VerificationError::Proof(err.into()))?;

        let mut transcript = Self::prepare_transcript(self.version, &self.source, self.fee, self.nonce);

        trace!("verifying commitments eq proofs");

        // This contains sender balance updated, output ciphertext, asset commitment
        let mut commitments_changes = Vec::new();

        for (commitment, new_source_commitment) in self
            .source_commitments
            .iter()
            .zip(&new_source_commitments_decompressed)
        {
            // Ciphertext containing all the funds spent for this commitment
            let output = self.get_sender_output_ct(commitment.get_asset(), &transfers_decompressed, &deposits_decompressed)
                .map_err(ProofVerificationError::from)?;

            // Retrieve the balance of the sender
            let mut source_verification_ciphertext = state
                .get_sender_balance(&self.source, commitment.get_asset(), &self.reference).await
                .map_err(VerificationError::State)?
                .clone();

            let source_ct_compressed = source_verification_ciphertext.compress();

            // Compute the new final balance for account
            source_verification_ciphertext -= &output;
            transcript.new_commitment_eq_proof_domain_separator();
            transcript.append_hash(b"new_source_commitment_asset", commitment.get_asset());
            transcript
                .append_commitment(b"new_source_commitment", &commitment.get_commitment());

            if self.version >= TxVersion::V0 {
                transcript.append_ciphertext(b"source_ct", &source_ct_compressed);
            }

            commitment.get_proof().pre_verify(
                &owner,
                &source_verification_ciphertext,
                &new_source_commitment,
                &mut transcript,
                &mut sigma_batch_collector,
            )?;

            commitments_changes.push((source_verification_ciphertext, output, commitment.get_asset()));
        }

        trace!("Verifying sigma proofs");
        sigma_batch_collector
            .verify()
            .map_err(|_| ProofVerificationError::GenericProof)?;

        // Proofs are correct, apply
        for (source_verification_ciphertext, output, asset) in commitments_changes {
            // Update sender final balance for asset
            let current_ciphertext = state
                .get_sender_balance(&self.source, asset, &self.reference)
                .await
                .map_err(VerificationError::State)?;
            *current_ciphertext = source_verification_ciphertext;

            // Update sender output for asset
            state
                .add_sender_output(
                    &self.source,
                    asset,
                    output,
                ).await
                .map_err(VerificationError::State)?;
        }

        self.apply(tx_hash, state, &deposits_decompressed).await
    }
}
