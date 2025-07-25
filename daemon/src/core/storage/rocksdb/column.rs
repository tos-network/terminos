use strum::{Display, EnumIter, AsRefStr};

const PREFIX_TOPOHEIGHT_LEN: usize = 8;
const PREFIX_ID_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Hash, EnumIter, Display, AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Column {
    // All transactions stored
    // {tx_hash} => {transaction}
    Transactions,
    // Which TXs are marked as executed
    // {tx_hash} => {block_hash}
    TransactionsExecuted,
    // In which blocks this TX was included
    // {tx_hash} => {block_hashes}
    TransactionInBlocks,
    // Transaction contract outputs
    // Standardized events that occurs on a contract call
    // {tx_hash} => {outputs}
    TransactionsOutputs,

    // ordered blocks hashes based on execution
    // {position} => {block_hash}
    BlocksExecutionOrder,
    // All blocks stored
    // {block_hash} => {block}
    Blocks,
    // All blocks hashes stored per height
    // {height} => {block_hashes}
    BlocksAtHeight,
    // Topoheight for a block hash
    // {block_hash} => {topoheight}
    TopoByHash,
    // Hash at a topoheight
    // {topoheight} => {block_hash}
    HashAtTopo,
    // Block difficulty / cumulative difficulty / covariance
    // {block_hash} => {difficulty}
    BlockDifficulty,
    // Misc data with no specific rules
    Common,
    // Topoheight Metadata
    // {topoheight} => {metadata}
    TopoHeightMetadata,

    // Each asset hash registered
    // {asset_hash} => {asset}
    Assets,
    // {asset_id} => {asset_hash}
    AssetById,
    // {topoheight}{asset_hash} => {asset}
    VersionedAssets,

    // {account_key} => {account}
    Account,
    // Column used as a "versioned" as its 
    // prefixed with a topoheight to have
    // easier search per topoheight
    // {topoheight}{account_key} => {}
    PrefixedRegistrations,
    // This column is used as a reverse index
    // {account_id} => {account_key}
    AccountById,

    // {topoheight}{account_id} => {version}
    VersionedMultisig,
    // {topoheight}{account_id} => {version}
    VersionedNonces,

    // Account balances pointer
    // {account_id}{asset_id} => {topoheight}
    Balances,
    // {topoheight}{account_id}{asset_id} => {version}
    VersionedBalances,

    // Contains the contract module per hash
    // {contract_hash} => {contract}
    Contracts,
    // {contract_id} => {contract_hash}
    ContractById,

    // {topoheight}{contract_id} => {version}
    VersionedContracts,
    // {topoheight}{contract_id}{data_key} => {version}
    VersionedContractsData,
    // Represent the link between a contract and a data
    // {contract_id}{data_key} => {topoheight}
    ContractsData,

    // A contract data accessible by its ID
    // {data_id} => {data}
    ContractDataById,

    // {contract}{asset} => {topoheight}
    ContractsBalances,
    // {topoheight}{contract}{asset} => {version}
    VersionedContractsBalances,

    // {topoheight}{asset_id} => {version}
    VersionedAssetsSupply,
    
    // Energy resources for each account
    // {account_key} => {topoheight}
    EnergyResources,
    // Versioned energy resources for each account
    // {topoheight}_{account_address} => {energy_resource}
    VersionedEnergyResources
}

impl Column {
    pub const fn prefix(&self) -> Option<usize> {
        use Column::*;

        match self {
            VersionedAssets
            | VersionedNonces
            | VersionedBalances
            | VersionedMultisig
            | VersionedAssetsSupply
            | VersionedContracts
            | VersionedContractsBalances
            | VersionedContractsData
            | PrefixedRegistrations
            | VersionedEnergyResources => Some(PREFIX_TOPOHEIGHT_LEN),

            ContractsBalances => Some(PREFIX_ID_LEN),
            Balances => Some(PREFIX_ID_LEN),

            _ => None,
        }
    }
}