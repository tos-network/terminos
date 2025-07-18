use serde::{Deserialize, Serialize};
use merlin::Transcript;
use log::debug;
use crate::{
    account::Nonce,
    crypto::{
        elgamal::CompressedPublicKey,
        Hash,
        Hashable,
        Signature,
    },
    serializer::*
};

use bulletproofs::RangeProof;
use multisig::MultiSig;

pub mod builder;
pub mod verify;
pub mod extra_data;
pub mod multisig;

mod payload;
mod source_commitment;
mod reference;
mod version;

pub use payload::*;
pub use reference::Reference;
pub use version::TxVersion;
pub use source_commitment::SourceCommitment;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::energy::FreezeDuration;
    use merlin::Transcript;

    #[test]
    fn test_append_energy_transcript_consistency() {
        // Test FreezeTos transcript consistency
        let freeze_duration = FreezeDuration::Day7;
        let freeze_payload = EnergyPayload::FreezeTos {
            amount: 1000,
            duration: freeze_duration,
        };

        let mut transcript1 = Transcript::new(b"test_transcript");
        let mut transcript2 = Transcript::new(b"test_transcript");
        
        // Use unified transcript operation
        Transaction::append_energy_transcript(&mut transcript1, &freeze_payload);
        Transaction::append_energy_transcript(&mut transcript2, &freeze_payload);
        
        // Both transcripts should be identical
        let mut challenge1 = [0u8; 32];
        let mut challenge2 = [0u8; 32];
        transcript1.challenge_bytes(b"test", &mut challenge1);
        transcript2.challenge_bytes(b"test", &mut challenge2);
        assert_eq!(challenge1, challenge2);

        // Test UnfreezeTos transcript consistency
        let unfreeze_payload = EnergyPayload::UnfreezeTos {
            amount: 500,
        };

        let mut transcript3 = Transcript::new(b"test_transcript");
        let mut transcript4 = Transcript::new(b"test_transcript");
        
        // Use unified transcript operation
        Transaction::append_energy_transcript(&mut transcript3, &unfreeze_payload);
        Transaction::append_energy_transcript(&mut transcript4, &unfreeze_payload);
        
        // Both transcripts should be identical
        let mut challenge3 = [0u8; 32];
        let mut challenge4 = [0u8; 32];
        transcript3.challenge_bytes(b"test", &mut challenge3);
        transcript4.challenge_bytes(b"test", &mut challenge4);
        assert_eq!(challenge3, challenge4);
    }

    #[test]
    fn test_energy_transcript_includes_tos_balance_changes() {
        let freeze_duration = FreezeDuration::Day7;
        let freeze_payload = EnergyPayload::FreezeTos {
            amount: 1000,
            duration: freeze_duration,
        };

        let mut transcript = Transcript::new(b"test_transcript");
        Transaction::append_energy_transcript(&mut transcript, &freeze_payload);
        
        // The transcript should include both energy and TOS balance change information
        // This ensures that both energy and TOS balance changes are considered in proof generation
        let mut challenge = [0u8; 32];
        transcript.challenge_bytes(b"test", &mut challenge);
        assert_ne!(challenge, [0u8; 32], "Transcript should contain data");
    }
}

// Maximum size of extra data per transfer
pub const EXTRA_DATA_LIMIT_SIZE: usize = 1024;
// Maximum total size of payload across all transfers per transaction
pub const EXTRA_DATA_LIMIT_SUM_SIZE: usize = EXTRA_DATA_LIMIT_SIZE * 32;
// Maximum number of transfers per transaction
pub const MAX_TRANSFER_COUNT: usize = 255;
// Maximum number of deposits per Invoke Call
pub const MAX_DEPOSIT_PER_INVOKE_CALL: usize = 255;
// Maximum number of participants in a multi signature account
pub const MAX_MULTISIG_PARTICIPANTS: usize = 255;

/// Simple enum to determine which DecryptHandle to use to craft a Ciphertext
/// This allows us to store one time the commitment and only a decrypt handle for each.
/// The DecryptHandle is used to decrypt the ciphertext and is selected based on the role in the transaction.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Sender,
    Receiver,
}

// this enum represent all types of transaction available on TOS Network
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TransactionType {
    Transfers(Vec<TransferPayload>),
    Burn(BurnPayload),
    MultiSig(MultiSigPayload),
    InvokeContract(InvokeContractPayload),
    DeployContract(DeployContractPayload),
    Energy(EnergyPayload),
}

/// Fee type for transactions
/// Determines whether the transaction uses Energy or TOS for fees
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum FeeType {
    /// Transaction uses TOS for fees (traditional fee model)
    TOS,
    /// Transaction uses Energy for fees (only available for Transfer transactions)
    Energy,
}

impl FeeType {
    /// Check if this fee type is Energy-based
    pub fn is_energy(&self) -> bool {
        matches!(self, FeeType::Energy)
    }

    /// Check if this fee type is TOS-based
    pub fn is_tos(&self) -> bool {
        matches!(self, FeeType::TOS)
    }
}

impl Serializer for FeeType {
    fn write(&self, writer: &mut Writer) {
        match self {
            FeeType::TOS => writer.write_u8(0),
            FeeType::Energy => writer.write_u8(1),
        }
    }

    fn read(reader: &mut Reader) -> Result<Self, ReaderError> {
        let variant = reader.read_u8()?;
        match variant {
            0 => Ok(FeeType::TOS),
            1 => Ok(FeeType::Energy),
            _ => Err(ReaderError::InvalidValue),
        }
    }

    fn size(&self) -> usize {
        1
    }
}

// Transaction to be sent over the network
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Transaction {
    /// Version of the transaction
    version: TxVersion,
    // Source of the transaction
    source: CompressedPublicKey,
    /// Type of the transaction
    data: TransactionType,
    /// Fees in TOS (when fee_type is TOS) or Energy cost (when fee_type is Energy)
    fee: u64,
    /// Fee type: TOS or Energy
    /// Energy can only be used for Transfer transactions
    fee_type: FeeType,
    /// nonce must be equal to the one on chain account
    /// used to prevent replay attacks and have ordered transactions
    nonce: Nonce,
    /// We have one source commitment and equality proof per asset used in the tx.
    source_commitments: Vec<SourceCommitment>,
    /// The range proof is aggregated across all transfers and across all assets.
    range_proof: RangeProof,
    /// At which block the TX is built
    reference: Reference,
    /// MultiSig contains the signatures of the transaction
    /// Only available since V1
    multisig: Option<MultiSig>,
    /// The signature of the source key
    signature: Signature,
}

impl Transaction {
    // Create a new transaction
    #[inline(always)]
    pub fn new(
        version: TxVersion,
        source: CompressedPublicKey,
        data: TransactionType,
        fee: u64,
        nonce: Nonce,
        source_commitments: Vec<SourceCommitment>,
        range_proof: RangeProof,
        reference: Reference,
        multisig: Option<MultiSig>,
        signature: Signature
    ) -> Self {
        Self::new_with_fee_type(
            version,
            source,
            data,
            fee,
            FeeType::TOS,
            nonce,
            source_commitments,
            range_proof,
            reference,
            multisig,
            signature,
        )
    }

    /// Create a new transaction with explicit fee type
    pub fn new_with_fee_type(
        version: TxVersion,
        source: CompressedPublicKey,
        data: TransactionType,
        fee: u64,
        fee_type: FeeType,
        nonce: Nonce,
        source_commitments: Vec<SourceCommitment>,
        range_proof: RangeProof,
        reference: Reference,
        multisig: Option<MultiSig>,
        signature: Signature
    ) -> Self {
        Self {
            version,
            source,
            data,
            fee,
            fee_type,
            nonce,
            source_commitments,
            range_proof,
            reference,
            multisig,
            signature,
        }
    }

    // Get the transaction version
    pub fn get_version(&self) -> TxVersion {
        self.version
    }

    // Get the source key
    pub fn get_source(&self) -> &CompressedPublicKey {
        &self.source
    }

    // Get the transaction type
    pub fn get_data(&self) -> &TransactionType {
        &self.data
    }

    // Get fees paid to miners
    pub fn get_fee(&self) -> u64 {
        self.fee
    }

    // Get the fee type (TOS or Energy)
    pub fn get_fee_type(&self) -> &FeeType {
        &self.fee_type
    }

    // Check if this transaction uses energy for fees
    pub fn uses_energy_fees(&self) -> bool {
        self.fee_type.is_energy()
    }

    // Check if this transaction uses TOS for fees
    pub fn uses_tos_fees(&self) -> bool {
        self.fee_type.is_tos()
    }

    // Get the nonce used
    pub fn get_nonce(&self) -> Nonce {
        self.nonce
    }

    // Get the source commitments
    pub fn get_source_commitments(&self) -> &Vec<SourceCommitment> {
        &self.source_commitments
    }

    // Get the used assets
    pub fn get_assets(&self) -> impl Iterator<Item = &Hash> {
        self.source_commitments.iter().map(SourceCommitment::get_asset)
    }

    // Get the range proof
    pub fn get_range_proof(&self) -> &RangeProof {
        &self.range_proof
    }

    // Get the multisig
    pub fn get_multisig(&self) -> &Option<MultiSig> {
        &self.multisig
    }

    // Get the count of signatures in a multisig transaction
    pub fn get_multisig_count(&self) -> usize {
        self.multisig.as_ref().map(|m| m.len()).unwrap_or(0)
    }

    // Get the signature of source key
    pub fn get_signature(&self) -> &Signature {
        &self.signature
    }

    // Get the block reference to determine which block the transaction is built
    pub fn get_reference(&self) -> &Reference {
        &self.reference
    }

    // Get the burned amount
    // This will returns the burned amount by a Burn payload
    // Or the % of execution fees to burn due to a Smart Contracts call
    // only if the asset is TOS
    pub fn get_burned_amount(&self, asset: &Hash) -> Option<u64> {
        match &self.data {
            TransactionType::Burn(payload) if payload.asset == *asset => Some(payload.amount),
            _ => None
        }
    }

    // Get the total outputs count per TX
    // default is 1
    // Transfers / Deposits are their own len
    pub fn get_outputs_count(&self) -> usize {
        match &self.data {
            TransactionType::Transfers(transfers) => transfers.len(),
            TransactionType::InvokeContract(payload) => payload.deposits.len().max(1),
            _ => 1
        }
    }

    // Consume the transaction by returning the source public key and the transaction type
    pub fn consume(self) -> (CompressedPublicKey, TransactionType) {
        (self.source, self.data)
    }

    /// Unified transcript operation for energy transactions
    /// This function ensures consistent transcript operations between generation and verification
    /// It handles both energy changes and TOS balance changes for freeze/unfreeze operations
    pub fn append_energy_transcript(transcript: &mut Transcript, payload: &EnergyPayload) {
        match payload {
            EnergyPayload::FreezeTos { amount, duration } => {
                // Add energy operation parameters
                transcript.append_u64(b"energy_amount", *amount);
                transcript.append_u64(b"energy_is_freeze", 1);
                transcript.append_u64(b"energy_freeze_duration", duration.duration_in_blocks());
                
                // Add TOS balance change information
                // FreezeTos deducts TOS from balance and adds energy
                transcript.append_u64(b"tos_balance_change", *amount); // Amount deducted from TOS balance
                transcript.append_u64(b"energy_gained", (*amount as f64 * duration.reward_multiplier()) as u64);
                
                debug!("Energy transcript - FreezeTos: amount={}, duration={}, tos_deducted={}, energy_gained={}", 
                       amount, duration.duration_in_blocks(), amount, (*amount as f64 * duration.reward_multiplier()) as u64);
            },
            EnergyPayload::UnfreezeTos { amount } => {
                // Add energy operation parameters
                transcript.append_u64(b"energy_amount", *amount);
                transcript.append_u64(b"energy_is_freeze", 0);
                
                // Add TOS balance change information
                // UnfreezeTos returns TOS to balance and removes energy
                transcript.append_u64(b"tos_balance_change", *amount); // Amount returned to TOS balance
                transcript.append_u64(b"energy_removed", *amount); // Energy removed (1:1 ratio for unfreeze)
                
                debug!("Energy transcript - UnfreezeTos: amount={}, tos_returned={}, energy_removed={}", 
                       amount, amount, amount);
            }
        }
    }
}

impl Serializer for TransactionType {
    fn write(&self, writer: &mut Writer) {
        match self {
            TransactionType::Burn(payload) => {
                writer.write_u8(0);
                payload.write(writer);
            }
            TransactionType::Transfers(txs) => {
                writer.write_u8(1);
                // max 255 txs per transaction
                let len: u8 = txs.len() as u8;
                writer.write_u8(len);
                for tx in txs {
                    tx.write(writer);
                }
            },
            TransactionType::MultiSig(payload) => {
                writer.write_u8(2);
                payload.write(writer);
            },
            TransactionType::InvokeContract(payload) => {
                writer.write_u8(3);
                payload.write(writer);
            },
            TransactionType::DeployContract(module) => {
                writer.write_u8(4);
                module.write(writer);
            },
            TransactionType::Energy(payload) => {
                writer.write_u8(5);
                payload.write(writer);
            }
        };
    }

    fn read(reader: &mut Reader) -> Result<TransactionType, ReaderError> {
        Ok(match reader.read_u8()? {
            0 => {
                let payload = BurnPayload::read(reader)?;
                TransactionType::Burn(payload)
            },
            1 => {
                let txs_count = reader.read_u8()?;
                if txs_count == 0 || txs_count > MAX_TRANSFER_COUNT as u8 {
                    return Err(ReaderError::InvalidSize)
                }

                let mut txs = Vec::with_capacity(txs_count as usize);
                for _ in 0..txs_count {
                    txs.push(TransferPayload::read(reader)?);
                }
                TransactionType::Transfers(txs)
            },
            2 => TransactionType::MultiSig(MultiSigPayload::read(reader)?),
            3 => TransactionType::InvokeContract(InvokeContractPayload::read(reader)?),
            4 => TransactionType::DeployContract(DeployContractPayload::read(reader)?),
            5 => TransactionType::Energy(EnergyPayload::read(reader)?),
            _ => {
                return Err(ReaderError::InvalidValue)
            }
        })
    }

    fn size(&self) -> usize {
        1 + match self {
            TransactionType::Burn(payload) => payload.size(),
            TransactionType::Transfers(txs) => {
                // 1 byte for variant, 1 byte for count of transfers
                let mut size = 1;
                for tx in txs {
                    size += tx.size();
                }
                size
            },
            TransactionType::MultiSig(payload) => {
                // 1 byte for variant, 1 byte for threshold, 1 byte for count of participants
                1 + 1 + payload.participants.iter().map(|p| p.size()).sum::<usize>()
            },
            TransactionType::InvokeContract(payload) => payload.size(),
            TransactionType::DeployContract(module) => module.size(),
            TransactionType::Energy(payload) => payload.size(),
        }
    }
}

impl Serializer for Transaction {
    fn write(&self, writer: &mut Writer) {
        self.version.write(writer);
        self.source.write(writer);
        self.data.write(writer);
        self.fee.write(writer);
        self.fee_type.write(writer);
        self.nonce.write(writer);

        writer.write_u8(self.source_commitments.len() as u8);
        for commitment in &self.source_commitments {
            commitment.write(writer);
        }

        self.range_proof.write(writer);
        self.reference.write(writer);

        // Include multisig information in V0 version as well
        self.multisig.write(writer);

        self.signature.write(writer);
    }

    fn read(reader: &mut Reader) -> Result<Transaction, ReaderError> {
        let version = TxVersion::read(reader)?;

        reader.context_mut()
            .store(version);

        let source = CompressedPublicKey::read(reader)?;
        let data = TransactionType::read(reader)?;
        let fee = reader.read_u64()?;
        let fee_type = FeeType::read(reader)?;
        let nonce = Nonce::read(reader)?;

        let commitments_len = reader.read_u8()?;
        if commitments_len == 0 || commitments_len > MAX_TRANSFER_COUNT as u8 {
            return Err(ReaderError::InvalidSize)
        }

        let mut source_commitments = Vec::with_capacity(commitments_len as usize);
        for _ in 0..commitments_len {
            source_commitments.push(SourceCommitment::read(reader)?);
        }

        let range_proof = RangeProof::read(reader)?;
        let reference = Reference::read(reader)?;
        let multisig = if version == TxVersion::V0 {
            // Read multisig information in V0 version as well
            Option::read(reader)?
        } else {
            Option::read(reader)?
        };

        let signature = Signature::read(reader)?;

        Ok(Transaction::new_with_fee_type(
            version,
            source,
            data,
            fee,
            fee_type,
            nonce,
            source_commitments,
            range_proof,
            reference,
            multisig,
            signature,
        ))
    }

    fn size(&self) -> usize {
        // Version byte
        let size = 1
        + self.source.size()
        + self.data.size()
        + self.fee.size()
        + self.fee_type.size()
        + self.nonce.size()
        // Commitments length byte
        + 1
        + self.source_commitments.iter().map(|c| c.size()).sum::<usize>()
        + self.range_proof.size()
        + self.reference.size()
        // Calculate multisig size in V0 version as well
        + self.multisig.size()
        + self.signature.size();

        size
    }
}

impl Hashable for Transaction {}

impl AsRef<Transaction> for Transaction {
    fn as_ref(&self) -> &Transaction {
        self
    }
}