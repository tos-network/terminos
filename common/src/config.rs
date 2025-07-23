use crate::{
    contract::register_opaque_types,
    crypto::Hash,
    static_assert
};

pub const VERSION: &str = env!("BUILD_VERSION");
pub const TERMINOS_ASSET: Hash = Hash::zero();

// ===== NEW TRON-STYLE ENERGY-BASED FEE MODEL =====

// Account activation fee (similar to TRON's 0.1 TRX)
// 0.1 TOS per account activation
pub const ACCOUNT_ACTIVATION_FEE: u64 = 10000000; // 0.1 TOS

// Energy-based fee model constants
// Only transfer operations consume energy
// Adjusted to match TRON's ratio: 1 TOS freeze = 7 free transfers/3 days
pub const ENERGY_PER_TRANSFER: u64 = 1;           // Basic transfer (1 energy per transfer)
pub const ENERGY_PER_KB: u64 = 10;                // Per KB of transaction data

// Energy to TOS conversion rate (when energy is insufficient)
// 1 energy = 0.0001 TOS (market rate)
pub const ENERGY_TO_TOS_RATE: u64 = 10000; // 0.0001 TOS per energy

// Legacy fee constants (kept for reference only)
// These are no longer used in the new energy-based model
pub const FEE_PER_KB: u64 = 10000;
pub const FEE_PER_ACCOUNT_CREATION: u64 = 100000;
pub const FEE_PER_TRANSFER: u64 = 5000;
pub const FEE_PER_MULTISIG_SIGNATURE: u64 = 500;

// Contracts rules
// 1 TOS per contract deployed
// Each contract deployed has a overhead of 1 TOS
// This amount is burned and is needed for safety of the chain
// Otherwise people could bloat the chain by deploying contracts
// And could make the chain unusable or slow
// Note that if we depends on fees only, miners could do such attacks for free
// by mining their own transactions and getting the fees back
pub const BURN_PER_CONTRACT: u64 = COIN_VALUE;
// 1 TOS per token created
// This is to prevent spamming the network with tokens
pub const COST_PER_TOKEN: u64 = COIN_VALUE;
// 30% of the transaction fee is burned
// This is to reduce the supply over time
// and also to prevent spamming the network with low fee transactions
// or free tx from miners
// This should be enabled once Smart Contracts are released
pub const TX_GAS_BURN_PERCENT: u64 = 30;
// Fee per store operation in a contract
// Each store operation has a fixed cost of 0.000001 TOS
pub const FEE_PER_STORE_CONTRACT: u64 = 100;
// Fee per byte of data stored in a contract
// Each byte of data stored (key + value) in a contract has a fixed cost
// 0.00000005 TOS per byte
pub const FEE_PER_BYTE_STORED_CONTRACT: u64 = 5;
// Fee per byte of data stored in a contract memory
// Each byte of data stored in the contract memory has a fixed cost
pub const FEE_PER_BYTE_IN_CONTRACT_MEMORY: u64 = 1;
// Fee per byte of data used to emit an event
// Data is not stored, but only exposed to websocket listeners
pub const FEE_PER_BYTE_OF_EVENT_DATA: u64 = 2;
// Max gas usage available per block
// Currently, set to 10 TOS per transaction
pub const MAX_GAS_USAGE_PER_TX: u64 = COIN_VALUE * 10;

// 8 decimals numbers
pub const COIN_DECIMALS: u8 = 8;
// 100 000 000 to represent 1 TOS
pub const COIN_VALUE: u64 = 10u64.pow(COIN_DECIMALS as u32);
// 18.4M full coin
pub const MAXIMUM_SUPPLY: u64 = 18_400_000 * COIN_VALUE;

// Addresses format
// mainnet prefix address
pub const PREFIX_ADDRESS: &str = "tos";
// testnet prefix address
pub const TESTNET_PREFIX_ADDRESS: &str = "tst";

// Proof prefix
pub const PREFIX_PROOF: &str = "proof";

// 1 KB = 1024 bytes
pub const BYTES_PER_KB: usize = 1024;

// Max transaction size in bytes
pub const MAX_TRANSACTION_SIZE: usize = BYTES_PER_KB * BYTES_PER_KB; // 1 MB

// Max block size in bytes
// 1024 * 1024 + (256 * 1024) bytes = 1.25 MB maximum size per block with txs
pub const MAX_BLOCK_SIZE: usize = (BYTES_PER_KB * BYTES_PER_KB) + (256 * BYTES_PER_KB);

// BlockDAG rules
pub const TIPS_LIMIT: usize = 3; // maximum 3 TIPS per block

// Initialize the configuration
pub fn init() {
    // register the opaque types
    register_opaque_types();
}


// Static checks
static_assert!(MAX_TRANSACTION_SIZE <= MAX_BLOCK_SIZE, "Max transaction size must be less than or equal to max block size");
static_assert!(MAXIMUM_SUPPLY >= COIN_VALUE, "Maximum supply must be greater than or equal to coin value");