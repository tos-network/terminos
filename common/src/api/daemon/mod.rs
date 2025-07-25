mod direction;

use std::{
    borrow::Cow,
    collections::{HashSet, HashMap},
    net::SocketAddr
};
use indexmap::IndexSet;
use serde::{
    Deserialize,
    Serialize,
    Serializer,
    Deserializer,
    de::Error
};
use terminos_vm::ValueCell;
use crate::{
    account::{Nonce, CiphertextCache, VersionedBalance, VersionedNonce},
    block::{TopoHeight, Algorithm, BlockVersion, EXTRA_NONCE_SIZE},
    crypto::{Address, Hash},
    difficulty::{CumulativeDifficulty, Difficulty},
    network::Network,
    time::{TimestampMillis, TimestampSeconds},
    transaction::extra_data::{SharedKey, UnknownExtraDataFormat},
};
use super::{default_true_value, DataElement, RPCContractOutput, RPCTransaction};

pub use direction::*;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub enum BlockType {
    Sync,
    Side,
    Orphaned,
    Normal
}

// Serialize the extra nonce in a hexadecimal string
pub fn serialize_extra_nonce<S: Serializer>(extra_nonce: &Cow<'_, [u8; EXTRA_NONCE_SIZE]>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&hex::encode(extra_nonce.as_ref()))
}

// Deserialize the extra nonce from a hexadecimal string
pub fn deserialize_extra_nonce<'de, 'a, D: Deserializer<'de>>(deserializer: D) -> Result<Cow<'a, [u8; EXTRA_NONCE_SIZE]>, D::Error> {
    let mut extra_nonce = [0u8; EXTRA_NONCE_SIZE];
    let hex = String::deserialize(deserializer)?;
    let decoded = hex::decode(hex).map_err(Error::custom)?;
    extra_nonce.copy_from_slice(&decoded);
    Ok(Cow::Owned(extra_nonce))
}

// Structure used to map the public key to a human readable address
#[derive(Serialize, Deserialize)]
pub struct RPCBlockResponse<'a> {
    pub hash: Cow<'a, Hash>,
    pub topoheight: Option<TopoHeight>,
    pub block_type: BlockType,
    pub difficulty: Cow<'a, Difficulty>,
    pub supply: Option<u64>,
    // Reward can be split into two parts
    pub reward: Option<u64>,
    // Miner reward (the one that found the block)
    pub miner_reward: Option<u64>,
    // And Dev Fee reward if enabled
    pub dev_reward: Option<u64>,
    pub cumulative_difficulty: Cow<'a, CumulativeDifficulty>,
    pub total_fees: Option<u64>,
    pub total_size_in_bytes: usize,
    pub version: BlockVersion,
    pub tips: Cow<'a, IndexSet<Hash>>,
    pub timestamp: TimestampMillis,
    pub height: u64,
    pub nonce: Nonce,
    #[serde(serialize_with = "serialize_extra_nonce")]
    #[serde(deserialize_with = "deserialize_extra_nonce")]
    pub extra_nonce: Cow<'a, [u8; EXTRA_NONCE_SIZE]>,
    pub miner: Cow<'a, Address>,
    pub txs_hashes: Cow<'a, IndexSet<Hash>>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
    )]
    pub transactions: Vec<RPCTransaction<'a>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMempoolParams {
    pub maximum: Option<usize>,
    pub skip: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MempoolTransactionSummary<'a> {
    // TX hash
    pub hash: Cow<'a, Hash>,
    // The current sender
    pub source: Address,
    // Fees expected to be paid
    pub fee: u64,
    // First time seen in the mempool
    pub first_seen: TimestampSeconds,
    // Size of the TX
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionSummary<'a> {
    // TX hash
    pub hash: Cow<'a, Hash>,
    // The current sender
    pub source: Address,
    // Fees expected to be paid
    pub fee: u64,
    // Size of the TX
    pub size: usize,
}

#[derive(Serialize, Deserialize)]
pub struct GetMempoolResult<'a> {
    // The range of transactions requested
    pub transactions: Vec<TransactionResponse<'a>>,
    // How many TXs in total available in mempool
    pub total: usize,
}

#[derive(Serialize, Deserialize)]
pub struct GetMempoolSummaryResult<'a> {
    // The range of transactions requested
    pub transactions: Vec<MempoolTransactionSummary<'a>>,
    // How many TXs in total available in mempool
    pub total: usize,
}

pub type BlockResponse = RPCBlockResponse<'static>;

#[derive(Serialize, Deserialize)]
pub struct GetTopBlockParams {
    #[serde(default)]
    pub include_txs: bool
}

#[derive(Serialize, Deserialize)]
pub struct GetBlockAtTopoHeightParams {
    pub topoheight: TopoHeight,
    #[serde(default)]
    pub include_txs: bool
}

#[derive(Serialize, Deserialize)]
pub struct GetBlocksAtHeightParams {
    pub height: u64,
    #[serde(default)]
    pub include_txs: bool
}

#[derive(Serialize, Deserialize)]
pub struct GetBlockByHashParams<'a> {
    pub hash: Cow<'a, Hash>,
    #[serde(default)]
    pub include_txs: bool
}

#[derive(Serialize, Deserialize)]
pub struct GetBlockTemplateParams<'a> {
    pub address: Cow<'a, Address>
}

#[derive(Serialize, Deserialize)]
pub struct GetMinerWorkParams<'a> {
    // Block Template in hexadecimal format
    pub template: Cow<'a, String>,
    // Address of the miner, if empty, it will use the address from template
    pub address: Option<Cow<'a, Address>>,
}

#[derive(Serialize, Deserialize)]
pub struct GetBlockTemplateResult {
    // block_template is Block Header in hexadecimal format
    // miner jobs can be created from it
    pub template: String,
    // Algorithm to use for the POW challenge
    pub algorithm: Algorithm,
    // Blockchain height
    pub height: u64,
    // Topoheight of the daemon
    pub topoheight: TopoHeight,
    // Difficulty target for the POW challenge
    pub difficulty: Difficulty,
}

#[derive(Serialize, Deserialize, PartialEq)]
pub struct GetMinerWorkResult {
    // algorithm to use
    pub algorithm: Algorithm,
    // template is miner job in hex format
    pub miner_work: String,
    // block height
    pub height: u64,
    // difficulty required for valid block POW
    pub difficulty: Difficulty,
    // topoheight of the daemon
    // this is for visual purposes only
    pub topoheight: TopoHeight,
}

#[derive(Serialize, Deserialize)]
pub struct SubmitMinerWorkParams {
    // hex: represent block miner in hexadecimal format
    // NOTE: alias block_template is used for backward compatibility < 1.9.4
    #[serde(alias = "miner_work", alias = "block_template")]
    pub miner_work: String,
}

#[derive(Serialize, Deserialize)]
pub struct SubmitBlockParams {
    // hex: represent the BlockHeader (Block)
    pub block_template: String,
    // optional miner work to apply to the block template
    pub miner_work: Option<String>
}

#[derive(Serialize, Deserialize)]
pub struct GetBalanceParams<'a> {
    pub address: Cow<'a, Address>,
    pub asset: Cow<'a, Hash>
}

#[derive(Serialize, Deserialize)]
pub struct HasBalanceParams<'a> {
    pub address: Cow<'a, Address>,
    pub asset: Cow<'a, Hash>,
    #[serde(default)]
    pub topoheight: Option<TopoHeight>
}

#[derive(Serialize, Deserialize)]
pub struct HasBalanceResult {
    pub exist: bool
}

#[derive(Serialize, Deserialize)]
pub struct GetBalanceAtTopoHeightParams<'a> {
    pub address: Cow<'a, Address>,
    pub asset: Cow<'a, Hash>,
    pub topoheight: TopoHeight
}

#[derive(Serialize, Deserialize)]
pub struct GetNonceParams<'a> {
    pub address: Cow<'a, Address>
}

#[derive(Serialize, Deserialize)]
pub struct HasNonceParams<'a> {
    pub address: Cow<'a, Address>,
    #[serde(default)]
    pub topoheight: Option<TopoHeight>
}

#[derive(Serialize, Deserialize)]
pub struct GetNonceAtTopoHeightParams<'a> {
    pub address: Cow<'a, Address>,
    pub topoheight: TopoHeight
}

#[derive(Serialize, Deserialize)]
pub struct GetNonceResult {
    pub topoheight: TopoHeight,
    #[serde(flatten)]
    pub version: VersionedNonce
}

#[derive(Serialize, Deserialize)]
pub struct HasNonceResult {
    pub exist: bool
}

#[derive(Serialize, Deserialize)]
pub struct GetBalanceResult {
    pub version: VersionedBalance,
    pub topoheight: TopoHeight
}

#[derive(Serialize, Deserialize)]
pub struct GetStableBalanceResult {
    pub version: VersionedBalance,
    pub stable_topoheight: TopoHeight,
    pub stable_block_hash: Hash 
}

#[derive(Serialize, Deserialize)]
pub struct GetInfoResult {
    pub height: u64,
    pub topoheight: TopoHeight,
    pub stableheight: u64,
    pub pruned_topoheight: Option<TopoHeight>,
    pub top_block_hash: Hash,
    // Current TOS circulating supply
    // This is calculated by doing
    // emitted_supply - burned_supply
    pub circulating_supply: u64,
    // Burned TOS supply
    #[serde(default)]
    pub burned_supply: u64,
    // Emitted TOS supply
    #[serde(default)]
    pub emitted_supply: u64,
    // Maximum supply of TOS
    pub maximum_supply: u64,
    // Current difficulty at tips
    pub difficulty: Difficulty,
    // Expected block time in milliseconds
    pub block_time_target: u64,
    // Average block time of last 50 blocks
    // in milliseconds
    pub average_block_time: u64,
    pub block_reward: u64,
    pub dev_reward: u64,
    pub miner_reward: u64,
    // count how many transactions are present in mempool
    pub mempool_size: usize,
    // software version on which the daemon is running
    pub version: String,
    // Network state (mainnet, testnet, devnet)
    pub network: Network,
    // Current block version enabled
    // Always returned by the daemon
    // But for compatibility with previous nodes
    // it is set to None
    pub block_version: Option<BlockVersion>,
}

#[derive(Serialize, Deserialize)]
pub struct SubmitTransactionParams {
    pub data: String // should be in hex format
}

#[derive(Serialize, Deserialize)]
pub struct GetTransactionParams<'a> {
    pub hash: Cow<'a, Hash>
}

pub type GetTransactionExecutorParams<'a> = GetTransactionParams<'a>;

#[derive(Serialize, Deserialize)]
pub struct GetTransactionExecutorResult<'a> {
    pub block_topoheight: TopoHeight,
    pub block_timestamp: TimestampMillis,
    pub block_hash: Cow<'a, Hash>
}

#[derive(Serialize, Deserialize)]
pub struct GetPeersResponse<'a> {
    // Peers that are connected and allows to be displayed
    pub peers: Vec<PeerEntry<'a>>,
    // All peers connected
    pub total_peers: usize,
    // Peers that asked to not be listed
    pub hidden_peers: usize
}

#[derive(Serialize, Deserialize)]
pub struct PeerEntry<'a> {
    pub id: u64,
    pub addr: Cow<'a, SocketAddr>,
    pub local_port: u16,
    pub tag: Cow<'a, Option<String>>,
    pub version: Cow<'a, String>,
    pub top_block_hash: Cow<'a, Hash>,
    pub topoheight: TopoHeight,
    pub height: u64,
    pub last_ping: TimestampSeconds,
    pub pruned_topoheight: Option<TopoHeight>,
    pub peers: Cow<'a, HashMap<SocketAddr, TimedDirection>>,
    pub cumulative_difficulty: Cow<'a, CumulativeDifficulty>,
    pub connected_on: TimestampSeconds,
    pub bytes_sent: usize,
    pub bytes_recv: usize,
}

#[derive(Serialize, Deserialize)]
pub struct P2pStatusResult<'a> {
    pub peer_count: usize,
    pub max_peers: usize,
    pub tag: Cow<'a, Option<String>>,
    pub our_topoheight: TopoHeight,
    pub best_topoheight: TopoHeight,
    pub median_topoheight: TopoHeight,
    pub peer_id: u64
}

#[derive(Serialize, Deserialize)]
pub struct GetTopoHeightRangeParams {
    pub start_topoheight: Option<TopoHeight>,
    pub end_topoheight: Option<TopoHeight>
}

#[derive(Serialize, Deserialize)]
pub struct GetHeightRangeParams {
    pub start_height: Option<u64>,
    pub end_height: Option<u64>
}

#[derive(Serialize, Deserialize)]
pub struct GetTransactionsParams {
    pub tx_hashes: Vec<Hash>
}

#[derive(Serialize, Deserialize)]
pub struct TransactionResponse<'a> {
    // in which blocks it was included
    pub blocks: Option<HashSet<Hash>>,
    // in which blocks it was executed
    pub executed_in_block: Option<Hash>,
    // if it is in mempool
    pub in_mempool: bool,
    // if its a mempool tx, we add the timestamp when it was added
    #[serde(default)]
    pub first_seen: Option<TimestampSeconds>,
    #[serde(flatten)]
    pub data: RPCTransaction<'a>
}

fn default_terminos_asset() -> Hash {
    crate::config::TERMINOS_ASSET
}

#[derive(Serialize, Deserialize)]
pub struct GetAccountHistoryParams {
    pub address: Address,
    #[serde(default = "default_terminos_asset")]
    pub asset: Hash,
    pub minimum_topoheight: Option<TopoHeight>,
    pub maximum_topoheight: Option<TopoHeight>,
    // Any incoming funds tracked
    #[serde(default = "default_true_value")]
    pub incoming_flow: bool,
    // Any outgoing funds tracked
    #[serde(default = "default_true_value")]
    pub outgoing_flow: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")] 
pub enum AccountHistoryType {
    DevFee { reward: u64 },
    Mining { reward: u64 },
    Burn { amount: u64 },
    Outgoing { to: Address },
    Incoming { from: Address },
    MultiSig {
        participants: Vec<Address>,
        threshold: u8,
    },
    InvokeContract {
        contract: Hash,
        chunk_id: u16,
    },
    // Contract hash is already stored
    // by the parent struct
    DeployContract,
    FreezeTos { amount: u64, duration: String },
    UnfreezeTos { amount: u64 },
}

#[derive(Serialize, Deserialize)]
pub struct AccountHistoryEntry {
    pub topoheight: TopoHeight,
    pub hash: Hash,
    #[serde(flatten)]
    pub history_type: AccountHistoryType,
    pub block_timestamp: TimestampMillis
}

#[derive(Serialize, Deserialize)]
pub struct GetAccountAssetsParams<'a> {
    pub address: Cow<'a, Address>,
    pub skip: Option<usize>,
    pub maximum: Option<usize>
}

#[derive(Serialize, Deserialize)]
pub struct GetAssetParams<'a> {
    pub asset: Cow<'a, Hash>
}

#[derive(Serialize, Deserialize)]
pub struct GetAssetsParams {
    pub skip: Option<usize>,
    pub maximum: Option<usize>,
    pub minimum_topoheight: Option<TopoHeight>,
    pub maximum_topoheight: Option<TopoHeight>
}

#[derive(Serialize, Deserialize)]
pub struct GetAccountsParams {
    pub skip: Option<usize>,
    pub maximum: Option<usize>,
    pub minimum_topoheight: Option<TopoHeight>,
    pub maximum_topoheight: Option<TopoHeight>
}

#[derive(Serialize, Deserialize)]
pub struct IsAccountRegisteredParams<'a> {
    pub address: Cow<'a, Address>,
    // If it is registered in stable height (confirmed)
    pub in_stable_height: bool,
}

#[derive(Serialize, Deserialize)]
pub struct GetAccountRegistrationParams<'a> {
    pub address: Cow<'a, Address>,
}

#[derive(Serialize, Deserialize)]
pub struct IsTxExecutedInBlockParams<'a> {
    pub tx_hash: Cow<'a, Hash>,
    pub block_hash: Cow<'a, Hash>
}

// Struct to define dev fee threshold
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DevFeeThreshold {
    // block height to start dev fee
    pub height: u64,
    // percentage of dev fee, example 10 = 10%
    pub fee_percentage: u64
}

// Struct to define hard fork
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct HardFork {
    // block height to start hard fork
    pub height: u64,
    // Block version to use
    pub version: BlockVersion,
    // All the changes that will be applied
    pub changelog: &'static str,
    // Version requirement, example: >=1.13.0
    // This is used for p2p protocol
    pub version_requirement: Option<&'static str>,
}

// Struct to returns the size of the blockchain on disk
#[derive(Serialize, Deserialize)]
pub struct SizeOnDiskResult {
    pub size_bytes: u64,
    pub size_formatted: String
}

#[derive(Serialize, Deserialize)]
pub struct GetMempoolCacheParams<'a> {
    pub address: Cow<'a, Address>
}

#[derive(Serialize, Deserialize)]
pub struct GetMempoolCacheResult {
    // lowest nonce used
    min: Nonce,
    // highest nonce used
    max: Nonce,
    // all txs ordered by nonce
    txs: Vec<Hash>,
    // All "final" cached balances used
    balances: HashMap<Hash, CiphertextCache>
}

// This struct is used to store the fee rate estimation for the following priority levels:
// 1. Low
// 2. Medium
// 3. High
// Each priority is in fee per KB.  It cannot be below `FEE_PER_KB` which is required by the network.
#[derive(Serialize, Deserialize)]
pub struct FeeRatesEstimated {
    pub low: u64,
    pub medium: u64,
    pub high: u64,
    // The minimum fee rate possible on the network
    pub default: u64
}

#[derive(Serialize, Deserialize)]
pub struct GetDifficultyResult {
    pub difficulty: Difficulty,
    pub hashrate: Difficulty,
    pub hashrate_formatted: String
}

#[derive(Serialize, Deserialize)]
pub struct ValidateAddressParams<'a> {
    pub address: Cow<'a, Address>,
    #[serde(default)]
    pub allow_integrated: bool,
    #[serde(default)]
    pub max_integrated_data_size: Option<usize>
}

#[derive(Serialize, Deserialize)]
pub struct ValidateAddressResult {
    pub is_valid: bool,
    pub is_integrated: bool
}

#[derive(Serialize, Deserialize)]
pub struct ExtractKeyFromAddressParams<'a> {
    pub address: Cow<'a, Address>,
    #[serde(default)]
    pub as_hex: bool
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractKeyFromAddressResult {
    Bytes(Vec<u8>),
    Hex(String)
}

#[derive(Serialize, Deserialize)]
pub struct MakeIntegratedAddressParams<'a> {
    pub address: Cow<'a, Address>,
    pub integrated_data: Cow<'a, DataElement>
}

#[derive(Serialize, Deserialize)]
pub struct DecryptExtraDataParams<'a> {
    pub shared_key: Cow<'a, SharedKey>,
    pub extra_data: Cow<'a, UnknownExtraDataFormat>
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultisigState {
    // If the user has deleted its multisig at requested topoheight
    Deleted,
    // If the user has a multisig at requested topoheight
    Active {
        participants: Vec<Address>,
        threshold: u8
    }
}

#[derive(Serialize, Deserialize)]
pub struct GetMultisigAtTopoHeightParams<'a> {
    pub address: Cow<'a, Address>,
    pub topoheight: TopoHeight
}


#[derive(Serialize, Deserialize)]
pub struct GetMultisigAtTopoHeightResult {
    pub state: MultisigState,
}

#[derive(Serialize, Deserialize)]
pub struct GetMultisigParams<'a> {
    pub address: Cow<'a, Address>
}

#[derive(Serialize, Deserialize)]
pub struct GetMultisigResult {
    // State at topoheight
    pub state: MultisigState,
    // Topoheight of the last change
    pub topoheight: TopoHeight
}

#[derive(Serialize, Deserialize)]
pub struct HasMultisigParams<'a> {
    pub address: Cow<'a, Address>
}


#[derive(Serialize, Deserialize)]
pub struct HasMultisigAtTopoHeightParams<'a> {
    pub address: Cow<'a, Address>,
    pub topoheight: TopoHeight
}

#[derive(Serialize, Deserialize)]
pub struct GetContractOutputsParams<'a> {
    pub transaction: Cow<'a, Hash>
}

#[derive(Serialize, Deserialize)]
pub struct GetContractModuleParams<'a> {
    pub contract: Cow<'a, Hash>
}

#[derive(Serialize, Deserialize)]
pub struct GetContractDataParams<'a> {
    pub contract: Cow<'a, Hash>,
    pub key: Cow<'a, ValueCell>
}

#[derive(Serialize, Deserialize)]
pub struct GetContractDataAtTopoHeightParams<'a> {
    pub contract: Cow<'a, Hash>,
    pub key: Cow<'a, ValueCell>,
    pub topoheight: TopoHeight
}

#[derive(Serialize, Deserialize)]
pub struct GetContractBalanceParams<'a> {
    pub contract: Cow<'a, Hash>,
    pub asset: Cow<'a, Hash>,
}

#[derive(Serialize, Deserialize)]
pub struct GetContractBalanceAtTopoHeightParams<'a> {
    pub contract: Cow<'a, Hash>,
    pub asset: Cow<'a, Hash>,
    pub topoheight: TopoHeight
}


#[derive(Serialize, Deserialize)]
pub struct GetContractBalancesParams<'a> {
    pub contract: Cow<'a, Hash>,
    pub skip: Option<usize>,
    pub maximum: Option<usize>
}

#[derive(Serialize, Deserialize)]
pub struct GetEnergyParams<'a> {
    pub address: Cow<'a, Address>
}

#[derive(Serialize, Deserialize)]
pub struct GetEnergyResult {
    pub frozen_tos: u64,
    pub total_energy: u64,
    pub used_energy: u64,
    pub available_energy: u64,
    pub last_update: u64,
    pub freeze_records: Vec<FreezeRecordInfo>,
}

#[derive(Serialize, Deserialize)]
pub struct FreezeRecordInfo {
    pub amount: u64,
    pub duration: String,
    pub freeze_topoheight: u64,
    pub unlock_topoheight: u64,
    pub energy_gained: u64,
    pub can_unlock: bool,
    pub remaining_blocks: u64,
}

#[derive(Serialize, Deserialize)]
pub struct RPCVersioned<T> {
    pub topoheight: TopoHeight,
    #[serde(flatten)]
    pub version: T
}

#[derive(Serialize, Deserialize)]
pub struct P2pBlockPropagationResult {
    // peer id => entry
    pub peers: HashMap<u64, TimedDirection>,
    // When was the first time we saw this block
    pub first_seen: Option<TimestampMillis>,
    // At which time we started to process it
    pub processing_at: Option<TimestampMillis>,
}

#[derive(Serialize, Deserialize)]
pub struct GetP2pBlockPropagation<'a> {
    pub hash: Cow<'a, Hash>,
    #[serde(default = "default_true_value")]
    pub outgoing: bool,
    #[serde(default = "default_true_value")]
    pub incoming: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyEvent {
    // When a new block is accepted by chain
    // it contains NewBlockEvent as value
    NewBlock,
    // When a block (already in chain or not) is ordered (new topoheight)
    // it contains BlockOrderedEvent as value
    BlockOrdered,
    // When a block that was ordered is not in the new DAG order
    // it contains BlockOrphanedEvent that got orphaned
    BlockOrphaned,
    // When stable height has changed (different than the previous one)
    // it contains StableHeightChangedEvent struct as value
    StableHeightChanged,
    // When stable topoheight has changed (different than the previous one)
    // it contains StableTopoHeightChangedEvent struct as value
    StableTopoHeightChanged,
    // When a transaction that was executed in a block is not reintroduced in mempool
    // It contains TransactionOrphanedEvent as value
    TransactionOrphaned,
    // When a new transaction is added in mempool
    // it contains TransactionAddedInMempoolEvent struct as value
    TransactionAddedInMempool,
    // When a transaction has been included in a valid block & executed on chain
    // it contains TransactionExecutedEvent struct as value
    TransactionExecuted,
    // When the contract has been invoked
    // This allows to track all the contract invocations
    InvokeContract {
        contract: Hash
    },
    // When a contract has transfered any token
    // to the receiver address
    // It contains ContractTransferEvent struct as value
    ContractTransfer {
        address: Address
    },
    // When a contract fire an event
    // It contains ContractEvent struct as value
    ContractEvent {
        // Contract hash to track
        contract: Hash,
        // ID of the event that is fired from the contract
        id: u64
    },
    // When a new contract has been deployed
    DeployContract,
    // When a new asset has been registered
    // It contains NewAssetEvent struct as value
    NewAsset,
    // When a new peer has connected to us
    // It contains PeerConnectedEvent struct as value
    PeerConnected,
    // When a peer has disconnected from us
    // It contains PeerDisconnectedEvent struct as value
    PeerDisconnected,
    // Peer peerlist updated, its all its connected peers
    // It contains PeerPeerListUpdatedEvent as value
    PeerPeerListUpdated,
    // Peer has been updated through a ping packet
    // Contains PeerStateUpdatedEvent as value
    PeerStateUpdated,
    // When a peer of a peer has disconnected
    // and that he notified us
    // It contains PeerPeerDisconnectedEvent as value
    PeerPeerDisconnected,
    // A new block template has been created
    NewBlockTemplate,
}

// Value of NotifyEvent::NewBlock
pub type NewBlockEvent = BlockResponse;

// Value of NotifyEvent::BlockOrdered
#[derive(Serialize, Deserialize)]
pub struct BlockOrderedEvent<'a> {
    // block hash in which this event was triggered
    pub block_hash: Cow<'a, Hash>,
    pub block_type: BlockType,
    // the new topoheight of the block
    pub topoheight: TopoHeight,
}

// Value of NotifyEvent::BlockOrphaned
#[derive(Serialize, Deserialize)]
pub struct BlockOrphanedEvent<'a> {
    pub block_hash: Cow<'a, Hash>,
    // Tpoheight of the block before being orphaned
    pub old_topoheight: TopoHeight
}

// Value of NotifyEvent::StableHeightChanged
#[derive(Serialize, Deserialize)]
pub struct StableHeightChangedEvent {
    pub previous_stable_height: u64,
    pub new_stable_height: u64
}

// Value of NotifyEvent::StableTopoHeightChanged
#[derive(Serialize, Deserialize)]
pub struct StableTopoHeightChangedEvent {
    pub previous_stable_topoheight: TopoHeight,
    pub new_stable_topoheight: TopoHeight
}


// Value of NotifyEvent::TransactionAddedInMempool
pub type TransactionAddedInMempoolEvent = MempoolTransactionSummary<'static>;
// Value of NotifyEvent::TransactionOrphaned
pub type TransactionOrphanedEvent = TransactionResponse<'static>;

// Value of NotifyEvent::TransactionExecuted
#[derive(Serialize, Deserialize)]
pub struct TransactionExecutedEvent<'a> {
    pub block_hash: Cow<'a, Hash>,
    pub tx_hash: Cow<'a, Hash>,
    pub topoheight: TopoHeight,
}

// Value of NotifyEvent::NewAsset
#[derive(Serialize, Deserialize)]
pub struct NewAssetEvent<'a> {
    pub asset: Cow<'a, Hash>,
    pub block_hash: Cow<'a, Hash>,
    pub topoheight: TopoHeight,
}

// Value of NotifyEvent::ContractTransfer
#[derive(Serialize, Deserialize)]
pub struct ContractTransferEvent<'a> {
    pub asset: Cow<'a, Hash>,
    pub amount: u64,
    pub block_hash: Cow<'a, Hash>,
    pub topoheight: TopoHeight,
}

// Value of NotifyEvent::ContractEvent
#[derive(Serialize, Deserialize)]
pub struct ContractEvent<'a> {
    pub data: Cow<'a, ValueCell>
}

// Value of NotifyEvent::PeerConnected
pub type PeerConnectedEvent = PeerEntry<'static>;

// Value of NotifyEvent::PeerDisconnected
pub type PeerDisconnectedEvent = PeerEntry<'static>;

// Value of NotifyEvent::PeerPeerListUpdated
#[derive(Serialize, Deserialize)]
pub struct PeerPeerListUpdatedEvent {
    // Peer ID of the peer that sent us the new peer list
    pub peer_id: u64,
    // Peerlist received from this peer
    pub peerlist: IndexSet<SocketAddr>
}

// Value of NotifyEvent::PeerStateUpdated
pub type PeerStateUpdatedEvent = PeerEntry<'static>;

// Value of NotifyEvent::PeerPeerDisconnected
#[derive(Serialize, Deserialize)]
pub struct PeerPeerDisconnectedEvent {
    // Peer ID of the peer that sent us this notification
    pub peer_id: u64,
    // address of the peer that disconnected from him
    pub peer_addr: SocketAddr
}

// Value of NotifyEvent::InvokeContract
#[derive(Serialize, Deserialize)]
pub struct InvokeContractEvent<'a> {
    pub block_hash: Cow<'a, Hash>,
    pub tx_hash: Cow<'a, Hash>,
    pub topoheight: TopoHeight,
    pub contract_outputs: Vec<RPCContractOutput<'a>>
}

// Value of NotifyEvent::NewContract
#[derive(Serialize, Deserialize)]
pub struct NewContractEvent<'a> {
    pub contract: Cow<'a, Hash>,
    pub block_hash: Cow<'a, Hash>,
    pub topoheight: TopoHeight,
}