use anyhow::Error;
use indexmap::IndexSet;
use lru::LruCache;
use serde_json::{Value, json};
use terminos_common::{
    api::{
        daemon::{
            BlockOrderedEvent,
            BlockOrphanedEvent,
            BlockType,
            NotifyEvent,
            StableHeightChangedEvent,
            StableTopoHeightChangedEvent,
            TransactionExecutedEvent,
            TransactionResponse,
            NewContractEvent,
            InvokeContractEvent,
            NewAssetEvent,
            ContractTransferEvent,
            ContractEvent,
        },
        RPCContractOutput,
        RPCTransaction
    },
    asset::{AssetData, VersionedAssetData},
    block::{
        Block,
        BlockHeader,
        BlockVersion,
        TopoHeight,
        EXTRA_NONCE_SIZE,
        get_combined_hash_for_tips
    },
    config::{
        COIN_DECIMALS,
        MAXIMUM_SUPPLY,
        MAX_TRANSACTION_SIZE,
        MAX_BLOCK_SIZE,
        TIPS_LIMIT,
        TERMINOS_ASSET
    },
    crypto::{
        Hash,
        Hashable,
        PublicKey,
        HASH_SIZE
    },
    difficulty::{
        check_difficulty,
        CumulativeDifficulty,
        Difficulty
    },
    immutable::Immutable,
    network::Network,
    serializer::Serializer,
    time::{
        get_current_time_in_millis,
        get_current_time_in_seconds,
        TimestampMillis
    },
    transaction::{
        verify::BlockchainVerificationState,
        Transaction,
        TransactionType
    },
    utils::{calculate_energy_fee, calculate_tx_fee, format_tos},
    tokio::{spawn_task, is_multi_threads_supported},
    varuint::VarUint,
    contract::build_environment,
};
use terminos_vm::Environment;
use crate::{
    config::{
        get_genesis_block_hash, get_hex_genesis_block, get_minimum_difficulty, get_difficulty_at_hard_fork,
        BLOCK_TIME_MILLIS, DEV_FEES, DEV_PUBLIC_KEY, EMISSION_SPEED_FACTOR, GENESIS_BLOCK_DIFFICULTY,
        MILLIS_PER_SECOND, SIDE_BLOCK_REWARD_MAX_BLOCKS, PRUNE_SAFETY_LIMIT,
        SIDE_BLOCK_REWARD_PERCENT, SIDE_BLOCK_REWARD_MIN_PERCENT, STABLE_LIMIT, TIMESTAMP_IN_FUTURE_LIMIT,
    },
    core::{
        config::Config,
        blockdag,
        difficulty,
        error::BlockchainError,
        mempool::Mempool,
        nonce_checker::NonceChecker,
        simulator::Simulator,
        storage::{DagOrderProvider, DifficultyProvider, Storage},
        tx_selector::{TxSelector, TxSelectorEntry},
        state::{ChainState, ApplicableChainState},
        hard_fork::*
    },
    p2p::P2pServer,
    rpc::{
        rpc::{
            get_block_type_for_block,
            get_block_response
        },
        DaemonRpcServer,
        SharedDaemonRpcServer
    }
};
use std::{
    borrow::Cow,
    collections::{
        HashMap,
        hash_map::Entry,
        HashSet,
        VecDeque
    },
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc
    },
    time::Instant
};
use tokio::{
    net::lookup_host,
    sync::{broadcast, Mutex, RwLock}
};
use log::{info, error, debug, warn, trace};
use rand::Rng;

use super::storage::{
    AccountProvider,
    BlocksAtHeightProvider,
    ClientProtocolProvider,
    PrunedTopoheightProvider,
};

#[derive(Debug, Clone, Copy)]
pub enum BroadcastOption {
    // P2P + Miners
    All,
    // GetWork
    Miners,
    // None of them
    None,
}

impl BroadcastOption {
    pub fn miners(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn p2p(&self) -> bool {
        matches!(self, Self::All)
    }
}

pub struct Blockchain<S: Storage> {
    // current block height
    height: AtomicU64,
    // current topo height
    topoheight: AtomicU64,
    // current stable height
    stable_height: AtomicU64,
    // Determine which last block is stable
    // It is used mostly for chain rewind limit
    stable_topoheight: AtomicU64,
    // mempool to retrieve/add all txs
    mempool: RwLock<Mempool>,
    // storage to retrieve/add blocks
    storage: RwLock<S>,
    // Contract environment stdlib
    environment: Environment,
    // P2p module
    p2p: RwLock<Option<Arc<P2pServer<S>>>>,
    // RPC module
    rpc: RwLock<Option<SharedDaemonRpcServer<S>>>,
    // current difficulty at tips
    // its used as cache to display current network hashrate
    difficulty: Mutex<Difficulty>,
    // if a simulator is set
    simulator: Option<Simulator>,
    // if we should skip PoW verification
    skip_pow_verification: bool,
    // Should we skip block template TXs verification
    skip_block_template_txs_verification: bool,
    // current network type on which one we're using/connected to
    network: Network,
    // this cache is used to avoid to recompute the common base for each block and is mandatory
    // key is (tip hash, tip height) while value is (base hash, base height)
    tip_base_cache: Mutex<LruCache<(Hash, u64), (Hash, u64)>>,
    // This cache is used to avoid to recompute the common base
    // key is a combined hash of tips
    common_base_cache: Mutex<LruCache<Hash, (Hash, u64)>>,
    // tip work score is used to determine the best tip based on a block, tip base ands a base height
    tip_work_score_cache: Mutex<LruCache<(Hash, Hash, u64), (HashSet<Hash>, CumulativeDifficulty)>>,
    // using base hash, current tip hash and base height, this cache is used to store the DAG order
    full_order_cache: Mutex<LruCache<(Hash, Hash, u64), IndexSet<Hash>>>,
    // auto prune mode if enabled, will delete all blocks every N and keep only N top blocks (topoheight based)
    auto_prune_keep_n_blocks: Option<u64>,
    // Blocks hashes checkpoints
    // No rewind can be done below these blocks
    checkpoints: HashSet<Hash>,
    // Threads count to use during a block verification
    // If more than one thread is used, it will use batch TXs
    // in differents groups and will verify them in parallel
    // If set to one, it will use the main thread directly
    txs_verification_threads_count: usize,
    // Force the DB to be flushed after each block added
    force_db_flush: bool,
}

impl<S: Storage> Blockchain<S> {
    pub async fn new(config: Config, network: Network, storage: S) -> Result<Arc<Self>, Error> {
        // Do some checks on config params
        {
            if config.simulator.is_some() && network != Network::Dev {
                error!("Impossible to enable simulator mode except in dev network!");
                return Err(BlockchainError::InvalidNetwork.into())
            }
    
            if let Some(keep_only) = config.auto_prune_keep_n_blocks {
                if keep_only < PRUNE_SAFETY_LIMIT {
                    error!("Auto prune mode should keep at least 80 blocks");
                    return Err(BlockchainError::AutoPruneMode.into())
                }
            }

            if config.p2p.allow_boost_sync && config.p2p.allow_fast_sync {
                error!("Boost sync and fast sync can't be enabled at the same time!");
                return Err(BlockchainError::ConfigSyncMode.into())
            }

            if config.skip_pow_verification {
                warn!("PoW verification is disabled! This is dangerous in production!");
            }

            if config.txs_verification_threads_count == 0 {
                error!("TXs threads count must be above 0");
                return Err(BlockchainError::InvalidConfig.into());
            } else {
                info!("Will use {} threads for TXs verification", config.txs_verification_threads_count);
            }

            if config.rpc.rpc_threads == 0 {
                error!("RPC threads count must be above 0");
                return Err(BlockchainError::InvalidConfig.into())
            }
        }

        let on_disk = storage.has_blocks().await;
        let (height, topoheight) = if on_disk {
            info!("Reading last metadata available...");
            let height = storage.get_top_height()?;
            let topoheight = storage.get_top_topoheight()?;

            (height, topoheight)
        } else { (0, 0) };

        let environment = build_environment::<S>().build();

        info!("Initializing chain...");
        let blockchain = Self {
            height: AtomicU64::new(height),
            topoheight: AtomicU64::new(topoheight),
            stable_height: AtomicU64::new(0),
            stable_topoheight: AtomicU64::new(0),
            mempool: RwLock::new(Mempool::new(network)),
            storage: RwLock::new(storage),
            environment,
            p2p: RwLock::new(None),
            rpc: RwLock::new(None),
            difficulty: Mutex::new(GENESIS_BLOCK_DIFFICULTY),
            skip_pow_verification: config.skip_pow_verification || config.simulator.is_some(),
            simulator: config.simulator,
            network,
            tip_base_cache: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
            tip_work_score_cache: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
            common_base_cache: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
            full_order_cache: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
            auto_prune_keep_n_blocks: config.auto_prune_keep_n_blocks,
            skip_block_template_txs_verification: config.skip_block_template_txs_verification,
            checkpoints: config.checkpoints.into_iter().collect(),
            txs_verification_threads_count: config.txs_verification_threads_count,
            force_db_flush: config.force_db_flush
        };

        // include genesis block
        if !on_disk {
            blockchain.create_genesis_block(config.genesis_block_hex.as_deref()).await?;
        } else if !config.recovery_mode {
            debug!("Retrieving tips for computing current difficulty");
            let mut storage = blockchain.get_storage().write().await;
            let tips = storage.get_tips().await?;
            let (difficulty, _) = blockchain.get_difficulty_at_tips(&*storage, tips.iter()).await?;
            blockchain.set_difficulty(difficulty).await;

            // now compute the stable height
            debug!("Retrieving tips for computing current stable height");
            let (stable_hash, stable_height) = blockchain.find_common_base::<S, _>(&storage, &tips).await?;
            blockchain.stable_height.store(stable_height, Ordering::SeqCst);
            // Search the stable topoheight
            let stable_topoheight = storage.get_topo_height_for_hash(&stable_hash).await?;
            blockchain.stable_topoheight.store(stable_topoheight, Ordering::SeqCst);

            // also do some clean up in case of DB corruption
            if config.check_db_integrity {
                info!("Cleaning data above topoheight {} in case of potential DB corruption", topoheight);
                storage.delete_versioned_data_above_topoheight(topoheight).await?;
            }
        } else {
            warn!("Recovery mode enabled, required pre-computed data have been skipped.");
        }

        let arc = Arc::new(blockchain);
        // create P2P Server
        if !config.p2p.disable_p2p_server {
            let dir_path = config.dir_path;
            let config = config.p2p;
            info!("Starting P2p server...");
            // setup exclusive nodes
            let mut exclusive_nodes: Vec<SocketAddr> = Vec::with_capacity(config.exclusive_nodes.len());
            for peer in config.exclusive_nodes {
                for peer in peer.split(",") {
                    match peer.parse() {
                        Ok(addr) => {
                            exclusive_nodes.push(addr);
                        }
                        Err(e) => {
                            match lookup_host(&peer).await {
                                Ok(it) => {
                                    info!("Valid host found for {}", peer);
                                    for addr in it {
                                        info!("IP from DNS resolution: {}", addr);
                                        exclusive_nodes.push(addr);
                                    }
                                },
                                Err(e2) => {
                                    error!("Error while parsing {} as exclusive node address: {}, {}", peer, e, e2);
                                }
                            };
                            continue;
                        }
                    };
                }
            }

            match P2pServer::new(
                config.p2p_concurrency_task_count_limit,
                dir_path,
                config.tag,
                config.max_peers,
                config.p2p_bind_address,
                Arc::clone(&arc),
                exclusive_nodes.is_empty(),
                exclusive_nodes,
                config.allow_fast_sync,
                config.allow_boost_sync,
                config.allow_priority_blocks,
                config.max_chain_response_size,
                !config.disable_ip_sharing,
                config.disable_p2p_outgoing_connections,
                config.p2p_dh_private_key.map(|v| v.into()),
                config.p2p_on_dh_key_change,
                config.p2p_stream_concurrency,
                config.p2p_temp_ban_duration.as_secs(),
                config.p2p_fail_count_limit
            ) {
                Ok(p2p) => {
                    // connect to priority nodes
                    for addr in config.priority_nodes {
                        for addr in addr.split(",") {
                            let addr: SocketAddr = match addr.parse() {
                                Ok(addr) => addr,
                                Err(e) => {
                                    match lookup_host(&addr).await {
                                        Ok(it) => {
                                            info!("Valid host found for {}", addr);
                                            for addr in it {
                                                info!("Trying to connect to priority node with IP from DNS resolution: {}", addr);
                                                p2p.try_to_connect_to_peer(addr, true).await;
                                            }
                                        },
                                        Err(e2) => {
                                            error!("Error while parsing {} as priority node address: {}, {}", addr, e, e2);
                                        }
                                    };
                                    continue;
                                }
                            };
                            info!("Trying to connect to priority node: {}", addr);
                            p2p.try_to_connect_to_peer(addr, true).await;
                        }
                    }
                    *arc.p2p.write().await = Some(p2p);
                },
                Err(e) => error!("Error while starting P2p server: {}", e)
            };
        }

        // create RPC Server
        if !config.rpc.disable_rpc_server {
            info!("RPC Server will listen on: {}", config.rpc.rpc_bind_address);
            match DaemonRpcServer::new(
                Arc::clone(&arc),
                config.rpc
            ).await {
                Ok(server) => *arc.rpc.write().await = Some(server),
                Err(e) => error!("Error while starting RPC server: {}", e)
            };
        }

        // Start the simulator task if necessary
        if let Some(simulator) = arc.simulator {
            warn!("Simulator {} mode enabled!", simulator);
            let blockchain = Arc::clone(&arc);
            spawn_task("simulator", async move {
                simulator.start(blockchain).await;
            });
        }

        Ok(arc)
    }

    // Detect if the simulator task has been started
    pub fn is_simulator_enabled(&self) -> bool {
        self.simulator.is_some()
    }

    // Skip PoW verification flag
    pub fn skip_pow_verification(&self) -> bool {
        self.skip_pow_verification
    }

    // get the environment stdlib for contract execution
    pub fn get_contract_environment(&self) -> &Environment {
        &self.environment
    }

    // Get the configured threads count for TXS
    pub fn get_txs_verification_threads_count(&self) -> usize {
        self.txs_verification_threads_count
    }

    // Stop all blockchain modules
    // Each module is stopped in its own context
    // So no deadlock occurs in case they are linked
    pub async fn stop(&self) {
        info!("Stopping modules...");
        {
            debug!("stopping p2p module");
            let mut p2p = self.p2p.write().await;
            if let Some(p2p) = p2p.take() {
                p2p.stop().await;
            }
        }

        {
            debug!("stopping rpc module");
            let mut rpc = self.rpc.write().await;
            if let Some(rpc) = rpc.take() {
                rpc.stop().await;
            }
        }

        {
            debug!("stopping storage module");
            let mut storage = self.storage.write().await;
            if let Err(e) = storage.stop().await {
                error!("Error while stopping storage: {}", e);
            }
        }

        {
            debug!("stopping mempool module");
            let mut mempool = self.mempool.write().await;
            mempool.stop().await;
        }

        info!("All modules are now stopped!");
    }

    // Clear all caches
    pub async fn clear_caches(&self) {
        debug!("Clearing caches...");
        self.tip_base_cache.lock().await.clear();
        self.tip_work_score_cache.lock().await.clear();
        self.common_base_cache.lock().await.clear();
        self.full_order_cache.lock().await.clear();
        debug!("Caches are now cleared!");
    }

    // Reload the storage and update all cache values
    // Clear the mempool also in case of not being up-to-date
    pub async fn reload_from_disk(&self) -> Result<(), BlockchainError> {
        trace!("Reloading chain from disk");
        let mut storage = self.storage.write().await;
        self.reload_from_disk_with_storage(&mut *storage).await
    }

    pub async fn reload_from_disk_with_storage(&self, storage: &mut S) -> Result<(), BlockchainError> {
        let topoheight = storage.get_top_topoheight()?;
        let height = storage.get_top_height()?;
        self.topoheight.store(topoheight, Ordering::SeqCst);
        self.height.store(height, Ordering::SeqCst);

        let tips = storage.get_tips().await?;
        // Research stable height to update caches
        let (stable_hash, stable_height) = self.find_common_base(&*storage, &tips).await?;
        self.stable_height.store(stable_height, Ordering::SeqCst);

        // Research stable topoheight also
        let stable_topoheight = storage.get_topo_height_for_hash(&stable_hash).await?;
        self.stable_topoheight.store(stable_topoheight, Ordering::SeqCst);

        // Recompute the difficulty with new tips
        let (difficulty, _) = self.get_difficulty_at_tips(&*storage, tips.iter()).await?;
        self.set_difficulty(difficulty).await;

        // TXs in mempool may be outdated, clear them as they will be asked later again
        debug!("locking mempool for cleaning");
        let mut mempool = self.mempool.write().await;
        debug!("Clearing mempool");
        mempool.clear();

        self.clear_caches().await;

        Ok(())
    }

    // function to include the genesis block and register the public dev key.
    async fn create_genesis_block(&self, genesis_hex: Option<&str>) -> Result<(), BlockchainError> {
        let mut storage = self.storage.write().await;

        // register TOS asset
        debug!("Registering TOS asset: {} at topoheight 0", TERMINOS_ASSET);
        let ticker = match self.network {
            Network::Mainnet => "TOS".to_owned(),
            _ => "TOT".to_owned(),
        };

        storage.add_asset(
            &TERMINOS_ASSET,
            0,
            VersionedAssetData::new(
                AssetData::new(COIN_DECIMALS, "TOS".to_owned(), ticker, Some(MAXIMUM_SUPPLY), None),
                None
            )
        ).await?;

        let (genesis_block, genesis_hash) = if let Some(genesis_block) = get_hex_genesis_block(&self.network) {
            info!("De-serializing genesis block for network {}...", self.network);
            let genesis = Block::from_hex(genesis_block)?;
            let expected_hash = genesis.hash();
            (genesis, expected_hash)
        } else if let Some(hex) = genesis_hex {
            info!("De-serializing genesis block hex from config...");
            let genesis = Block::from_hex(hex)?;
            let expected_hash = genesis.hash();

            (genesis, expected_hash)
        } else {
            warn!("No genesis block found!");
            info!("Generating a new genesis block...");
            let version = get_version_at_height(&self.network, 0);
            let header = BlockHeader::new(version, 0, get_current_time_in_millis(), IndexSet::new(), [0u8; EXTRA_NONCE_SIZE], DEV_PUBLIC_KEY.clone(), IndexSet::new());
            let block = Block::new(Immutable::Owned(header), Vec::new());
            let block_hash = block.hash();
            info!("Genesis generated: {} with {:?} {}", block.to_hex(), block_hash, block_hash);
            (block, block_hash)
        };

        if *genesis_block.get_miner() != *DEV_PUBLIC_KEY {
            return Err(BlockchainError::GenesisBlockMiner)
        }

        if let Some(expected_hash) = get_genesis_block_hash(&self.network) {
            if genesis_hash != *expected_hash {
                error!("Genesis block hash is invalid! Expected: {}, got: {}", expected_hash, genesis_hash);
                return Err(BlockchainError::InvalidGenesisHash)
            }
        }
        debug!("Adding genesis block '{}' to chain", genesis_hash);

        // hardcode genesis block topoheight
        storage.set_topo_height_for_block(&genesis_hash, 0).await?;
        storage.set_top_height(0)?;

        self.add_new_block_for_storage(&mut *storage, genesis_block, None, BroadcastOption::Miners, false).await?;

        Ok(())
    }

    // mine a block for current difficulty
    // This is for testing purpose and shouldn't be directly used as it will mine on async threads
    // which will reduce performance of the daemon and can take forever if difficulty is high
    pub async fn mine_block(&self, key: &PublicKey) -> Result<Block, BlockchainError> {
        let (mut header, difficulty) = {
            let storage = self.storage.read().await;
            let block = self.get_block_template_for_storage(&storage, key.clone()).await?;
            let (difficulty, _) = self.get_difficulty_at_tips(&*storage, block.get_tips().iter()).await?;
            (block, difficulty)
        };
        let algorithm = get_pow_algorithm_for_version(header.get_version());
        let mut hash = header.get_pow_hash(algorithm)?;
        let mut current_height = self.get_height();
        while !self.is_simulator_enabled() && !check_difficulty(&hash, &difficulty)? {
            if self.get_height() != current_height {
                current_height = self.get_height();
                header = self.get_block_template(key.clone()).await?;
            }
            header.nonce += 1;
            header.timestamp = get_current_time_in_millis();
            hash = header.get_pow_hash(algorithm)?;
        }

        let block = self.build_block_from_header(Immutable::Owned(header)).await?;
        let block_height = block.get_height();
        debug!("Mined a new block {} at height {}", hash, block_height);
        Ok(block)
    }

    // Prune the chain until topoheight
    // This will delete all blocks / versioned balances / txs until topoheight in param
    pub async fn prune_until_topoheight(&self, topoheight: TopoHeight) -> Result<TopoHeight, BlockchainError> {
        trace!("prune until topoheight {}", topoheight);
        let mut storage = self.storage.write().await;
        self.prune_until_topoheight_for_storage(topoheight, &mut *storage).await
    }

    // delete all blocks / versioned balances / txs until topoheight in param
    // for this, we have to locate the nearest Sync block for DAG under the limit topoheight
    // and then delete all blocks before it
    // keep a marge of PRUNE_SAFETY_LIMIT
    pub async fn prune_until_topoheight_for_storage(&self, topoheight: TopoHeight, storage: &mut S) -> Result<TopoHeight, BlockchainError> {
        if topoheight == 0 {
            return Err(BlockchainError::PruneZero)
        }

        let current_topoheight = self.get_topo_height();
        if topoheight >= current_topoheight || current_topoheight - topoheight < PRUNE_SAFETY_LIMIT {
            return Err(BlockchainError::PruneHeightTooHigh)
        }

        // 1 is to not delete the genesis block
        let last_pruned_topoheight = storage.get_pruned_topoheight().await?.unwrap_or(1);
        if topoheight < last_pruned_topoheight {
            return Err(BlockchainError::PruneLowerThanLastPruned)
        }

        // find new stable point based on a sync block under the limit topoheight
        let start = Instant::now();
        let located_sync_topoheight = self.locate_nearest_sync_block_for_topoheight(storage, topoheight, self.get_height()).await?;
        debug!("Located sync topoheight found {} in {}ms", located_sync_topoheight, start.elapsed().as_millis());

        if located_sync_topoheight > last_pruned_topoheight {
            // delete all blocks until the new topoheight
            let start = Instant::now();
            for topoheight in last_pruned_topoheight..located_sync_topoheight {
                trace!("Pruning block at topoheight {}", topoheight);
                // delete block
                let _ = storage.delete_block_at_topoheight(topoheight).await?;
            }
            debug!("Pruned blocks until topoheight {} in {}ms", located_sync_topoheight, start.elapsed().as_millis());

            let start = Instant::now();
            // delete balances for all assets
            // TODO: this is currently going through ALL data, we need to only detect changes made in last..located
            storage.delete_versioned_data_below_topoheight(located_sync_topoheight, true).await?;
            debug!("Pruned versioned data until topoheight {} in {}ms", located_sync_topoheight, start.elapsed().as_millis());

            // Update the pruned topoheight
            storage.set_pruned_topoheight(located_sync_topoheight).await?;
            Ok(located_sync_topoheight)
        } else {
            debug!("located_sync_topoheight <= topoheight, no pruning needed");
            Ok(last_pruned_topoheight)
        }
    }

    // determine the topoheight of the nearest sync block until limit topoheight
    pub async fn locate_nearest_sync_block_for_topoheight<P>(&self, provider: &P, mut topoheight: TopoHeight, current_height: u64) -> Result<TopoHeight, BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider + BlocksAtHeightProvider + PrunedTopoheightProvider
    {
        while topoheight > 0 {
            let block_hash = provider.get_hash_at_topo_height(topoheight).await?;
            if self.is_sync_block_at_height(provider, &block_hash, current_height).await? {
                let topoheight = provider.get_topo_height_for_hash(&block_hash).await?;
                return Ok(topoheight)
            }

            topoheight -= 1;
        }

        // genesis block is always a sync block
        Ok(0)
    }

    // returns the highest (unstable) height on the chain
    pub fn get_height(&self) -> u64 {
        self.height.load(Ordering::Acquire)
    }

    // returns the highest topological height
    pub fn get_topo_height(&self) -> TopoHeight {
        self.topoheight.load(Ordering::Acquire)
    }

    // Get the current block height stable
    // No blocks can be added at or below this height
    pub fn get_stable_height(&self) -> u64 {
        self.stable_height.load(Ordering::Acquire)
    }

    // Get the stable topoheight
    // It is used to determine at which DAG topological height
    // the block is in case of rewind
    pub fn get_stable_topoheight(&self) -> TopoHeight {
        self.stable_topoheight.load(Ordering::Acquire)
    }

    // Get the network on which this chain is running
    pub fn get_network(&self) -> &Network {
        &self.network
    }

    // Get the current emitted supply of TOS at current topoheight
    pub async fn get_supply(&self) -> Result<u64, BlockchainError> {
        trace!("get supply");
        let storage = self.storage.read().await;
        storage.get_supply_at_topo_height(self.get_topo_height()).await
    }

    // Get the current burned supply of TOS at current topoheight
    pub async fn get_burned_supply(&self) -> Result<u64, BlockchainError> {
        trace!("get burned supply");
        let storage = self.storage.read().await;
        storage.get_burned_supply_at_topo_height(self.get_topo_height()).await
    }

    // Get the count of transactions available in the mempool
    pub async fn get_mempool_size(&self) -> usize {
        trace!("get mempool size");
        self.mempool.read().await.size()
    }

    // Get the current top block hash in chain
    pub async fn get_top_block_hash(&self) -> Result<Hash, BlockchainError> {
        trace!("get top block hash");
        let storage = self.storage.read().await;
        self.get_top_block_hash_for_storage(&storage).await
    }

    // because we are in chain, we already now the highest topoheight
    // we call the get_hash_at_topo_height instead of get_top_block_hash to avoid reading value
    // that we already know
    pub async fn get_top_block_hash_for_storage(&self, storage: &S) -> Result<Hash, BlockchainError> {
        storage.get_hash_at_topo_height(self.get_topo_height()).await
    }

    // Verify if we have the current block in storage by locking it ourself
    pub async fn has_block(&self, hash: &Hash) -> Result<bool, BlockchainError> {
        trace!("has block {} in chain", hash);
        let storage = self.storage.read().await;
        storage.has_block_with_hash(hash).await
    }

    // Verify if the block is a sync block for current chain height
    pub async fn is_sync_block<P: DifficultyProvider + DagOrderProvider + BlocksAtHeightProvider + PrunedTopoheightProvider>(&self, provider: &P, hash: &Hash) -> Result<bool, BlockchainError> {
        let current_height = self.get_height();
        self.is_sync_block_at_height(provider, hash, current_height).await
    }

    // Verify if the block is a sync block
    // A sync block is a block that is ordered and has the highest cumulative difficulty at its height
    // It is used to determine if the block is a stable block or not
    async fn is_sync_block_at_height<P>(&self, provider: &P, hash: &Hash, height: u64) -> Result<bool, BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider + BlocksAtHeightProvider + PrunedTopoheightProvider
    {
        trace!("is sync block {} at height {}", hash, height);
        let block_height = provider.get_height_for_block_hash(hash).await?;
        if block_height == 0 { // genesis block is a sync block
            trace!("Block {} at height {} is a sync block because it can only be the genesis block", hash, block_height);
            return Ok(true)
        }

        // block must be ordered and in stable height
        if block_height + STABLE_LIMIT > height || !provider.is_block_topological_ordered(hash).await {
            trace!("Block {} at height {} is not a sync block, it is not in stable height", hash, block_height);
            return Ok(false)
        }

        // We are only pruning at sync block
        if let Some(pruned_topo) = provider.get_pruned_topoheight().await? {
            let topoheight = provider.get_topo_height_for_hash(hash).await?;
            if pruned_topo == topoheight {
                // We only prune at sync block, if block is pruned, it is a sync block
                trace!("Block {} at height {} is a sync block, it is pruned", hash, block_height);
                return Ok(true)
            }
        }

        // if block is alone at its height, it is a sync block
        let tips_at_height = provider.get_blocks_at_height(block_height).await?;
        // This may be an issue with orphaned blocks, we can't rely on this
        // if tips_at_height.len() == 1 {
        //     return Ok(true)
        // }

        // if block is not alone at its height and they are ordered (not orphaned), it can't be a sync block
        for hash_at_height in tips_at_height {
            if *hash != hash_at_height && provider.is_block_topological_ordered(&hash_at_height).await {
                trace!("Block {} at height {} is not a sync block, it has more than 1 block at its height", hash, block_height);
                return Ok(false)
            }
        }

        // now lets check all blocks until STABLE_LIMIT height before the block
        let stable_point = if block_height >= STABLE_LIMIT {
            block_height - STABLE_LIMIT
        } else {
            STABLE_LIMIT - block_height
        };
        let mut i = block_height - 1;
        let mut pre_blocks = HashSet::new();
        while i >= stable_point && i != 0 {
            let blocks = provider.get_blocks_at_height(i).await?;
            pre_blocks.extend(blocks);
            i -= 1;
        }

        let sync_block_cumulative_difficulty = provider.get_cumulative_difficulty_for_block_hash(hash).await?;
        // if potential sync block has lower cumulative difficulty than one of past blocks, it is not a sync block
        for pre_hash in pre_blocks {
            // We compare only against block ordered otherwise we can have desync between node which could lead to fork
            // This is rare event but can happen
            if provider.is_block_topological_ordered(&pre_hash).await {
                let cumulative_difficulty = provider.get_cumulative_difficulty_for_block_hash(&pre_hash).await?;
                if cumulative_difficulty >= sync_block_cumulative_difficulty {
                    warn!("Block {} at height {} is not a sync block, it has lower cumulative difficulty than block {} at height {}", hash, block_height, pre_hash, i);
                    return Ok(false)
                }
            }
        }

        trace!("block {} at height {} is a sync block", hash, block_height);

        Ok(true)
    }

    async fn find_tip_base<P>(&self, provider: &P, hash: &Hash, height: u64, pruned_topoheight: TopoHeight) -> Result<(Hash, u64), BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider + BlocksAtHeightProvider + PrunedTopoheightProvider
    {
        debug!("Finding tip base for {} at height {}", hash, height);
        let mut cache = self.tip_base_cache.lock().await;
        debug!("tip base cache locked for {} at height {}", hash, height);

        let mut stack: VecDeque<Hash> = VecDeque::new();
        stack.push_back(hash.clone());

        let mut bases: IndexSet<(Hash, u64)> = IndexSet::new();
        let mut processed = HashSet::new();

        'main: while let Some(current_hash) = stack.pop_back() {
            trace!("Finding tip base for {} at height {}", current_hash, height);
            processed.insert(current_hash.clone());
            if pruned_topoheight > 0 && provider.is_block_topological_ordered(&current_hash).await {
                let topoheight = provider.get_topo_height_for_hash(&current_hash).await?;
                // Node is pruned, we only prune chain to stable height / sync block so we can return the hash
                if topoheight <= pruned_topoheight {
                    let block_height = provider.get_height_for_block_hash(&current_hash).await?;
                    debug!("Node is pruned, returns tip {} at {} as stable tip base", current_hash, block_height);
                    bases.insert((current_hash.clone(), block_height));
                    continue 'main;
                }
            }

            // first, check if we have it in cache
            if let Some((base_hash, base_height)) = cache.get(&(current_hash.clone(), height)) {
                trace!("Tip Base for {} at height {} found in cache: {} for height {}", current_hash, height, base_hash, base_height);
                bases.insert((base_hash.clone(), *base_height));
                continue 'main;
            }

            let tips = provider.get_past_blocks_for_block_hash(&current_hash).await?;
            let tips_count = tips.len();
            if tips_count == 0 { // only genesis block can have 0 tips saved
                // save in cache
                cache.put((hash.clone(), height), (current_hash.clone(), height));
                bases.insert((current_hash.clone(), 0));
                continue 'main;
            }

            for tip_hash in tips.iter() {
                if pruned_topoheight > 0 && provider.is_block_topological_ordered(&tip_hash).await {
                    let topoheight = provider.get_topo_height_for_hash(&tip_hash).await?;
                    // Node is pruned, we only prune chain to stable height / sync block so we can return the hash
                    if topoheight <= pruned_topoheight {
                        let block_height = provider.get_height_for_block_hash(&tip_hash).await?;
                        debug!("Node is pruned, returns tip {} at {} as stable tip base", tip_hash, block_height);
                        bases.insert((tip_hash.clone(), block_height));
                        continue 'main;
                    }
                }

                // if block is sync, it is a tip base
                if self.is_sync_block_at_height(provider, &tip_hash, height).await? {
                    let block_height = provider.get_height_for_block_hash(&tip_hash).await?;
                    // save in cache
                    cache.put((hash.clone(), height), (tip_hash.clone(), block_height));
                    bases.insert((tip_hash.clone(), block_height));
                    continue 'main;
                }

                if !processed.contains(tip_hash) {
                    // Tip was not sync, we need to find its tip base too
                    stack.push_back(tip_hash.clone());
                }
            }
        }

        if bases.is_empty() {
            error!("Tip base for {} at height {} not found", hash, height);
            return Err(BlockchainError::ExpectedTips)
        }

        // now we sort descending by height and return the last element deleted
        bases.sort_by(|(_, a), (_, b)| b.cmp(a));
        debug_assert!(bases[0].1 >= bases[bases.len() - 1].1);

        let (base_hash, base_height) = bases.pop().ok_or(BlockchainError::ExpectedTips)?;

        // save in cache
        cache.put((hash.clone(), height), (base_hash.clone(), base_height));
        trace!("Tip Base for {} at height {} found: {} for height {}", hash, height, base_hash, base_height);

        Ok((base_hash, base_height))
    }

    // find the common base (block hash and block height) of all tips
    pub async fn find_common_base<'a, P, I>(&self, provider: &P, tips: I) -> Result<(Hash, u64), BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider + BlocksAtHeightProvider + PrunedTopoheightProvider,
        I: IntoIterator<Item = &'a Hash> + Copy,
    {
        debug!("Searching for common base for tips {}", tips.into_iter().map(|h| h.to_string()).collect::<Vec<String>>().join(", "));
        let mut cache = self.common_base_cache.lock().await;
        debug!("common base cache locked");

        let combined_tips = get_combined_hash_for_tips(tips.into_iter());
        if let Some((hash, height)) = cache.get(&combined_tips) {
            debug!("Common base found in cache: {} at height {}", hash, height);
            return Ok((hash.clone(), *height))
        }

        let mut best_height = 0;
        // first, we check the best (highest) height of all tips
        for hash in tips.into_iter() {
            let height = provider.get_height_for_block_hash(hash).await?;
            if height > best_height {
                best_height = height;
            }
        }

        let pruned_topoheight = provider.get_pruned_topoheight().await?.unwrap_or(0);
        let mut bases = Vec::new();
        for hash in tips.into_iter() {
            trace!("Searching tip base for {}", hash);
            bases.push(self.find_tip_base(provider, hash, best_height, pruned_topoheight).await?);
        }

        // check that we have at least one value
        if bases.is_empty() {
            error!("bases list is empty");
            return Err(BlockchainError::ExpectedTips)
        }

        // sort it descending by height
        // a = 5, b = 6, b.cmp(a) -> Ordering::Greater
        bases.sort_by(|(_, a), (_, b)| b.cmp(a));
        debug_assert!(bases[0].1 >= bases[bases.len() - 1].1);

        // retrieve the first block hash with its height
        // we delete the last element because we sorted it descending
        // and we want the lowest height
        let (base_hash, base_height) = bases.remove(bases.len() - 1);
        debug!("Common base {} with height {} on {}", base_hash, base_height, bases.len() + 1);

        // save in cache
        cache.put(combined_tips, (base_hash.clone(), base_height));

        Ok((base_hash, base_height))
    }

    async fn build_reachability<P: DifficultyProvider>(&self, provider: &P, hash: Hash) -> Result<HashSet<Hash>, BlockchainError> {
        let mut set = HashSet::new();
        let mut stack: VecDeque<(Hash, u64)> = VecDeque::new();
        stack.push_back((hash, 0));
    
        while let Some((current_hash, current_level)) = stack.pop_back() {
            if current_level >= 2 * STABLE_LIMIT {
                trace!("Level limit reached, adding {}", current_hash);
                set.insert(current_hash);
            } else {
                trace!("Level {} reached with hash {}", current_level, current_hash);
                let tips = provider.get_past_blocks_for_block_hash(&current_hash).await?;
                set.insert(current_hash);
                for past_hash in tips.iter() {
                    if !set.contains(past_hash) {
                        stack.push_back((past_hash.clone(), current_level + 1));
                    }
                }
            }
        }

        Ok(set)
    }

    // this function check that a TIP cannot be refered as past block in another TIP
    async fn verify_non_reachability<P: DifficultyProvider>(&self, provider: &P, tips: &IndexSet<Hash>) -> Result<bool, BlockchainError> {
        trace!("Verifying non reachability for block");
        let tips_count = tips.len();
        let mut reach = Vec::with_capacity(tips_count);
        for hash in tips {
            let set = self.build_reachability(provider, hash.clone()).await?;
            reach.push(set);
        }

        for i in 0..tips_count {
            for j in 0..tips_count {
                // if a tip can be referenced as another's past block, its not a tip
                if i != j && reach[j].contains(&tips[i]) {
                    debug!("Tip {} (index {}) is reachable from tip {} (index {})", tips[i], i, tips[j], j);
                    trace!("reach: {}", reach[j].iter().map(|x| x.to_string()).collect::<Vec<String>>().join(", "));
                    return Ok(false)
                }
            }
        }
        Ok(true)
    }

    // Search the lowest height available from the tips of a block hash
    // We go through all tips and their tips until we have no unordered block left
    async fn find_lowest_height_from_mainchain<P>(&self, provider: &P, hash: Hash) -> Result<u64, BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider
    {
        // Lowest height found from mainchain
        let mut lowest_height = u64::max_value();
        // Current stack of blocks to process
        let mut stack: VecDeque<Hash> = VecDeque::new();
        // Because several blocks can have the same tips,
        // prevent to process a block twice
        let mut processed = HashSet::new();

        stack.push_back(hash);

        while let Some(current_hash) = stack.pop_back() {
            if processed.contains(&current_hash) {
                continue;
            }

            let tips = provider.get_past_blocks_for_block_hash(&current_hash).await?;
            for tip_hash in tips.iter() {
                if provider.is_block_topological_ordered(tip_hash).await {
                    let height = provider.get_height_for_block_hash(tip_hash).await?;
                    if lowest_height > height {
                        lowest_height = height;
                    }
                } else {
                    stack.push_back(tip_hash.clone());
                }
            }
            processed.insert(current_hash);
        }

        Ok(lowest_height)
    }

    // Search the lowest height available from this block hash
    // This function is used to calculate the distance from mainchain
    // It will recursively search all tips and their height
    // If a tip is not ordered, we will search its tips until we find an ordered block
    async fn calculate_distance_from_mainchain<P>(&self, provider: &P, hash: &Hash) -> Result<u64, BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider
    {
        if provider.is_block_topological_ordered(hash).await {
            let height = provider.get_height_for_block_hash(hash).await?;
            debug!("calculate_distance: Block {} is at height {}", hash, height);
            return Ok(height)
        }
        debug!("calculate_distance: Block {} is not ordered, calculate distance from mainchain", hash);
        let lowest_height = self.find_lowest_height_from_mainchain(provider, hash.clone()).await?;

        debug!("calculate_distance: lowest height found is {}", lowest_height);
        Ok(lowest_height)
    }

    // Verify if the block is not too far from mainchain
    // We calculate the distance from mainchain and compare it to the height
    async fn verify_distance_from_mainchain<P>(&self, provider: &P, hash: &Hash, height: u64) -> Result<bool, BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider
    {
        let distance = self.calculate_distance_from_mainchain(provider, hash).await?;
        Ok(!(distance <= height && height - distance >= STABLE_LIMIT))
    }

    // Find tip work score internal for a block hash
    // this will recursively find all tips and their difficulty
    async fn find_tip_work_score_internal<'a, P>(&self, provider: &P, map: &mut HashMap<Hash, CumulativeDifficulty>, hash: &'a Hash, base_topoheight: TopoHeight) -> Result<(), BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider
    {
        trace!("Finding tip work score for {}", hash);

        let mut stack: VecDeque<Hash> = VecDeque::new();
        stack.push_back(hash.clone());

        while let Some(current_hash) = stack.pop_back() {
            let tips = provider.get_past_blocks_for_block_hash(&current_hash).await?;

            for tip_hash in tips.iter() {
                if !map.contains_key(tip_hash) {
                    let is_ordered = provider.is_block_topological_ordered(tip_hash).await;
                    if !is_ordered || (is_ordered && provider.get_topo_height_for_hash(tip_hash).await? >= base_topoheight) {
                        stack.push_back(tip_hash.clone());
                    }
                }
            }

            if !map.contains_key(&current_hash) {
                map.insert(current_hash.clone(), provider.get_difficulty_for_block_hash(&current_hash).await?.into());
            }
        }
    
        Ok(())
    }

    // find the sum of work done
    pub async fn find_tip_work_score<P>(&self, provider: &P, hash: &Hash, base: &Hash, base_height: u64) -> Result<(HashSet<Hash>, CumulativeDifficulty), BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider
    {
        let mut cache = self.tip_work_score_cache.lock().await;
        if let Some(value) = cache.get(&(hash.clone(), base.clone(), base_height)) {
            trace!("Found tip work score in cache: set [{}], height: {}", value.0.iter().map(|h| h.to_string()).collect::<Vec<String>>().join(", "), value.1);
            return Ok(value.clone())
        }

        let tips = provider.get_past_blocks_for_block_hash(hash).await?;
        let mut map: HashMap<Hash, CumulativeDifficulty> = HashMap::new();
        let base_topoheight = provider.get_topo_height_for_hash(base).await?;
        for hash in tips.iter() {
            if !map.contains_key(hash) {
                let is_ordered = provider.is_block_topological_ordered(hash).await;
                if !is_ordered || (is_ordered && provider.get_topo_height_for_hash(hash).await? >= base_topoheight) {
                    self.find_tip_work_score_internal(provider, &mut map, hash, base_topoheight).await?;
                }
            }
        }

        if base != hash {
            map.insert(base.clone(), provider.get_cumulative_difficulty_for_block_hash(base).await?);
        }
        map.insert(hash.clone(), provider.get_difficulty_for_block_hash(hash).await?.into());

        let mut set = HashSet::with_capacity(map.len());
        let mut score = CumulativeDifficulty::zero();
        for (hash, value) in map {
            set.insert(hash);
            score += value;
        }

        // save this result in cache
        cache.put((hash.clone(), base.clone(), base_height), (set.clone(), score));

        Ok((set, score))
    }

    // find the best tip (highest cumulative difficulty)
    // We get their cumulative difficulty and sort them then take the first one
    async fn find_best_tip<'a, P: DifficultyProvider + DagOrderProvider>(&self, provider: &P, tips: &'a HashSet<Hash>, base: &Hash, base_height: u64) -> Result<&'a Hash, BlockchainError> {
        if tips.len() == 0 {
            return Err(BlockchainError::ExpectedTips)
        }

        let mut scores = Vec::with_capacity(tips.len());
        for hash in tips {
            let (_, cumulative_difficulty) = self.find_tip_work_score(provider, hash, base, base_height).await?;
            scores.push((hash, cumulative_difficulty));
        }

        blockdag::sort_descending_by_cumulative_difficulty(&mut scores);
        let (best_tip, _) = scores[0];
        Ok(best_tip)
    }

    // this function generate a DAG paritial order into a full order using recursive calls.
    // hash represents the best tip (biggest cumulative difficulty)
    // base represents the block hash of a block already ordered and in stable height
    // the full order is re generated each time a new block is added based on new TIPS
    // first hash in order is the base hash
    // base_height is only used for the cache key
    async fn generate_full_order<P>(&self, provider: &P, hash: &Hash, base: &Hash, base_height: u64, base_topo_height: TopoHeight) -> Result<IndexSet<Hash>, BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider
    {
        trace!("Generating full order for {} with base {}", hash, base);
        let mut cache = self.full_order_cache.lock().await;

        // Full order that is generated
        let mut full_order = IndexSet::new();
        // Current stack of hashes that need to be processed
        let mut stack: VecDeque<Hash> = VecDeque::new();
        stack.push_back(hash.clone());

        // Keep track of processed hashes that got reinjected for correct order
        let mut processed = IndexSet::new();

        'main: while let Some(current_hash) = stack.pop_back() {
            // If it is processed and got reinjected, its to maintains right order
            // We just need to insert current hash as it the "final hash" that got processed
            // after all tips
            if processed.contains(&current_hash) {
                full_order.insert(current_hash);
                continue 'main;
            }

            // Search in the cache to retrieve faster the full order
            let cache_key = (current_hash.clone(), base.clone(), base_height);
            if let Some(order_cache) = cache.get(&cache_key) {
                full_order.extend(order_cache.clone());
                continue 'main;
            }

            // Retrieve block tips
            let block_tips = provider.get_past_blocks_for_block_hash(&current_hash).await?;

            // if the block is genesis or its the base block, we can add it to the full order
            if block_tips.is_empty() || current_hash == *base {
                let mut order = IndexSet::new();
                order.insert(current_hash.clone());
                cache.put(cache_key, order.clone());
                full_order.extend(order);
                continue 'main;
            }

            // Calculate the score for each tips above the base topoheight
            let mut scores = Vec::new();
            for tip_hash in block_tips.iter() {
                let is_ordered = provider.is_block_topological_ordered(tip_hash).await;
                if !is_ordered || (is_ordered && provider.get_topo_height_for_hash(tip_hash).await? >= base_topo_height) {
                    let diff = provider.get_cumulative_difficulty_for_block_hash(tip_hash).await?;
                    scores.push((tip_hash.clone(), diff));
                } else {
                    debug!("Block {} is skipped in generate_full_order, is ordered = {}, base topo height = {}", tip_hash, is_ordered, base_topo_height);
                }
            }

            // We sort by ascending cumulative difficulty because it is faster
            // than doing a .reverse() on scores and give correct order for tips processing
            // using our stack impl 
            blockdag::sort_ascending_by_cumulative_difficulty(&mut scores);

            processed.insert(current_hash.clone());
            stack.push_back(current_hash);

            for (tip_hash, _) in scores {
                stack.push_back(tip_hash);
            }
        }

        cache.put((hash.clone(), base.clone(), base_height), full_order.clone());

        Ok(full_order)
    }

    // confirms whether the actual tip difficulty is withing 9% deviation with best tip (reference)
    async fn validate_tips<P: DifficultyProvider>(&self, provider: &P, best_tip: &Hash, tip: &Hash) -> Result<bool, BlockchainError> {
        const MAX_DEVIATION: Difficulty = Difficulty::from_u64(91);
        const PERCENTAGE: Difficulty = Difficulty::from_u64(100);

        let best_difficulty = provider.get_difficulty_for_block_hash(best_tip).await?;
        let block_difficulty = provider.get_difficulty_for_block_hash(tip).await?;

        Ok(best_difficulty * MAX_DEVIATION / PERCENTAGE < block_difficulty)
    }

    // Get difficulty at tips
    // If tips is empty, returns genesis difficulty
    // Find the best tip (highest cumulative difficulty), then its difficulty, timestamp and its own tips
    // Same for its parent, then calculate the difficulty between the two timestamps
    // For Block C, take the timestamp and difficulty from parent block B, and then from parent of B, take the timestamp
    // We take the difficulty from the biggest tip, but compute the solve time from the newest tips
    pub async fn get_difficulty_at_tips<'a, P, I>(&self, provider: &P, tips: I) -> Result<(Difficulty, VarUint), BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider + PrunedTopoheightProvider,
        I: IntoIterator<Item = &'a Hash> + ExactSizeIterator + Clone,
        I::IntoIter: ExactSizeIterator
    {
        // Get the height at the tips
        let height = blockdag::calculate_height_at_tips(provider, tips.clone().into_iter()).await?;

        // Get the version at the current height
        let (has_hard_fork, version) = has_hard_fork_at_height(self.get_network(), height);

        if tips.len() == 0 { // Genesis difficulty
            return Ok((GENESIS_BLOCK_DIFFICULTY, difficulty::get_covariance_p(version)))
        }

        // Simulator is enabled, don't calculate difficulty
        if height <= 1 || self.is_simulator_enabled() || has_hard_fork {
            return Ok((get_difficulty_at_hard_fork(self.get_network(), version), difficulty::get_covariance_p(version)))
        }

        // Search the highest difficulty available
        let best_tip = blockdag::find_best_tip_by_cumulative_difficulty(provider, tips.clone().into_iter()).await?;
        let biggest_difficulty = provider.get_difficulty_for_block_hash(best_tip).await?;

        // Search the newest tip available to determine the real solve time
        let (_, newest_tip_timestamp) = blockdag::find_newest_tip_by_timestamp(provider, tips.clone().into_iter()).await?;

        // Find the newest tips parent timestamp
        let parent_tips = provider.get_past_blocks_for_block_hash(best_tip).await?;
        let (_, parent_newest_tip_timestamp) = blockdag::find_newest_tip_by_timestamp(provider, parent_tips.iter()).await?;

        let p = provider.get_estimated_covariance_for_block_hash(best_tip).await?;

        // Get the minimum difficulty configured
        let minimum_difficulty = get_minimum_difficulty(self.get_network());

        let (difficulty, p_new) = difficulty::calculate_difficulty(parent_newest_tip_timestamp, newest_tip_timestamp, biggest_difficulty, p, minimum_difficulty, version);
        Ok((difficulty, p_new))
    }

    // Store the difficulty cache for the latest block
    async fn set_difficulty(&self, difficulty: Difficulty) {
        let mut lock = self.difficulty.lock().await;
        *lock = difficulty;
    }

    // Get the current difficulty target for the next block
    pub async fn get_difficulty(&self) -> Difficulty {
        *self.difficulty.lock().await
    }

    // pass in params the already computed block hash and its tips
    // check the difficulty calculated at tips
    // if the difficulty is valid, returns it (prevent to re-compute it)
    pub async fn verify_proof_of_work<'a, P, I>(&self, provider: &P, hash: &Hash, tips: I) -> Result<(Difficulty, VarUint), BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider + PrunedTopoheightProvider,
        I: IntoIterator<Item = &'a Hash> + ExactSizeIterator + Clone,
        I::IntoIter: ExactSizeIterator
    {
        trace!("Verifying proof of work for block {}", hash);
        let (difficulty, p) = self.get_difficulty_at_tips(provider, tips).await?;
        trace!("Difficulty at tips: {}", difficulty);
        if check_difficulty(hash, &difficulty)? {
            Ok((difficulty, p))
        } else {
            Err(BlockchainError::InvalidDifficulty)
        }
    }

    // Returns the P2p module used for blockchain if enabled
    pub fn get_p2p(&self) -> &RwLock<Option<Arc<P2pServer<S>>>> {
        &self.p2p
    }

    // Returns the RPC server used for blockchain if enabled
    pub fn get_rpc(&self) -> &RwLock<Option<SharedDaemonRpcServer<S>>> {
        &self.rpc
    }

    // Returns the storage used for blockchain
    pub fn get_storage(&self) -> &RwLock<S> {
        &self.storage
    }

    // Returns the blockchain mempool used
    pub fn get_mempool(&self) -> &RwLock<Mempool> {
        &self.mempool
    }

    // Add a tx to the mempool, its hash will be computed
    pub async fn add_tx_to_mempool(&self, tx: Transaction, broadcast: bool) -> Result<(), BlockchainError> {
        let hash = tx.hash();
        self.add_tx_to_mempool_with_hash(tx, Immutable::Owned(hash), broadcast).await
    }

    // Add a tx to the mempool with the given hash, it is not computed and the TX is transformed into an Arc
    pub async fn add_tx_to_mempool_with_hash(&self, tx: Transaction, hash: Immutable<Hash>, broadcast: bool) -> Result<(), BlockchainError> {
        trace!("add tx to mempool with hash");
        let storage = self.storage.read().await;
        self.add_tx_to_mempool_with_storage_and_hash(&storage, Arc::new(tx), hash, broadcast).await
    }

    // Add a tx to the mempool with the given hash, it will verify the TX and check that it is not already in mempool or in blockchain
    // and its validity (nonce, balance, etc...)
    pub async fn add_tx_to_mempool_with_storage_and_hash(&self, storage: &S, tx: Arc<Transaction>, hash: Immutable<Hash>, broadcast: bool) -> Result<(), BlockchainError> {
        let tx_size = tx.size();
        if tx_size > MAX_TRANSACTION_SIZE {
            return Err(BlockchainError::TxTooBig(tx_size, MAX_TRANSACTION_SIZE))
        }

        let hash = {
            let mut mempool = self.mempool.write().await;

            if mempool.contains_tx(&hash) {
                return Err(BlockchainError::TxAlreadyInMempool(hash.into_owned()))
            }

            // check that the TX is not already in blockchain
            if storage.is_tx_executed_in_a_block(&hash)? {
                return Err(BlockchainError::TxAlreadyInBlockchain(hash.into_owned()))
            }

            let stable_topoheight = self.get_stable_topoheight();
            let current_topoheight = self.get_topo_height();
            // get the highest nonce available
            // if presents, it means we have at least one tx from this owner in mempool
            if let Some(cache) = mempool.get_cache_for(tx.get_source()) {
                // we accept to delete a tx from mempool if the new one has a higher fee
                if let Some(hash) = cache.has_tx_with_same_nonce(tx.get_nonce()) {
                    // A TX with the same nonce is already in mempool
                    return Err(BlockchainError::TxNonceAlreadyUsed(tx.get_nonce(), hash.as_ref().clone()))
                }

                // check that the nonce is in the range
                if !(tx.get_nonce() <= cache.get_max() + 1 && tx.get_nonce() >= cache.get_min()) {
                    debug!("TX {} nonce is not in the range of the pending TXs for this owner, received: {}, expected between {} and {}", hash, tx.get_nonce(), cache.get_min(), cache.get_max());
                    return Err(BlockchainError::InvalidTxNonceMempoolCache(tx.get_nonce(), cache.get_min(), cache.get_max()))
                }
            }

            // Put the hash behind an Arc to share it cheaply
            let hash = hash.to_arc();

            let version = get_version_at_height(self.get_network(), self.get_height());
            mempool.add_tx(storage, &self.environment, stable_topoheight, current_topoheight, hash.clone(), tx.clone(), tx_size, version).await?;

            hash
        };

        if broadcast {
            // P2p broadcast to others peers
            if let Some(p2p) = self.p2p.read().await.as_ref() {
                let p2p = p2p.clone();
                let hash = hash.clone();
                spawn_task("tx-notify-p2p", async move {
                    p2p.broadcast_tx_hash(hash).await;
                });
            }

            // broadcast to websocket this tx
            if let Some(rpc) = self.rpc.read().await.as_ref() {
                // Notify miners if getwork is enabled
                if let Some(getwork) = rpc.getwork_server() {
                    let getwork = getwork.clone();
                    spawn_task("tx-notify-new-job", async move {
                        if let Err(e) = getwork.get_handler().notify_new_job_rate_limited().await {
                            debug!("Error while notifying miners for new tx: {}", e);
                        }
                    });
                }

                if rpc.is_event_tracked(&NotifyEvent::TransactionAddedInMempool).await {
                    let data = RPCTransaction::from_tx(&tx, &hash, storage.is_mainnet());
                    let data: TransactionResponse<'_> = TransactionResponse {
                        blocks: None,
                        executed_in_block: None,
                        in_mempool: true,
                        first_seen: Some(get_current_time_in_seconds()),
                        data,
                    };
                    let json = json!(data);

                    let rpc = rpc.clone();
                    spawn_task("rpc-notify-tx", async move {
                        if let Err(e) = rpc.notify_clients(&NotifyEvent::TransactionAddedInMempool, json).await {
                            debug!("Error while broadcasting event TransactionAddedInMempool to websocket: {}", e);
                        }
                    });
                }
            }
        }
        
        Ok(())
    }

    // Get a block template for the new block work (mining)
    pub async fn get_block_template(&self, address: PublicKey) -> Result<BlockHeader, BlockchainError> {
        trace!("get block template");
        let storage = self.storage.read().await;
        self.get_block_template_for_storage(&storage, address).await
    }

    // check that the TX Hash is present in mempool or in chain disk
    pub async fn has_tx(&self, hash: &Hash) -> Result<bool, BlockchainError> {
        trace!("has tx {}", hash);
        // check in mempool first
        // if its present, returns it
        {
            let mempool = self.mempool.read().await;
            if mempool.contains_tx(hash) {
                return Ok(true)
            }
        }

        // check in storage now
        trace!("has tx {} storage", hash);
        let storage = self.storage.read().await;
        storage.has_transaction(hash).await
    }

    // retrieve the TX based on its hash by searching in mempool then on disk
    pub async fn get_tx(&self, hash: &Hash) -> Result<Arc<Transaction>, BlockchainError> {
        trace!("get tx {} from blockchain", hash);
        // check in mempool first
        // if its present, returns it
        {
            trace!("Locking mempool for get tx {}", hash);
            let mempool = self.mempool.read().await;
            trace!("Mempool locked for get tx {}", hash);
            if let Ok(tx) = mempool.get_tx(hash) {
                return Ok(tx)
            } 
        }

        // check in storage now
        debug!("get tx {} lock", hash);
        let storage = self.storage.read().await;
        debug!("get tx {} lock acquired", hash);
        storage.get_transaction(hash).await
    }

    pub async fn get_block_header_template(&self, address: PublicKey) -> Result<BlockHeader, BlockchainError> {
        debug!("get block header template");
        let storage = self.storage.read().await;
        debug!("get block header template lock acquired");
        self.get_block_header_template_for_storage(&storage, address).await
    }

    // Generate a block header template without transactions
    pub async fn get_block_header_template_for_storage(&self, storage: &S, address: PublicKey) -> Result<BlockHeader, BlockchainError> {
        trace!("get block header template");
        let extra_nonce: [u8; EXTRA_NONCE_SIZE] = rand::thread_rng().gen::<[u8; EXTRA_NONCE_SIZE]>(); // generate random bytes
        let tips_set = storage.get_tips().await?;
        let mut tips = Vec::with_capacity(tips_set.len());
        for hash in tips_set {
            trace!("Tip found from storage: {}", hash);
            tips.push(hash);
        }

        let current_height = self.get_height();
        if tips.len() > 1 {
            let best_tip = blockdag::find_best_tip_by_cumulative_difficulty(storage, tips.iter()).await?.clone();
            debug!("Best tip selected for this block template is {}", best_tip);
            let mut selected_tips = Vec::with_capacity(tips.len());
            for hash in tips {
                if best_tip != hash {
                    if !self.validate_tips(storage, &best_tip, &hash).await? {
                        warn!("Tip {} is invalid, not selecting it because difficulty can't be less than 91% of {}", hash, best_tip);
                        continue;
                    }

                    if !self.verify_distance_from_mainchain(storage, &hash, current_height).await? {
                        warn!("Tip {} is not selected for mining: too far from mainchain at height: {}", hash, current_height);
                        continue;
                    }
                }
                selected_tips.push(hash);
            }
            tips = selected_tips;

            if tips.is_empty() {
                warn!("No valid tips found for block template, using best tip {}", best_tip);
                tips.push(best_tip);
            }
        }

        let mut sorted_tips = blockdag::sort_tips(storage, tips.into_iter()).await?;
        if sorted_tips.len() > TIPS_LIMIT {
            let dropped_tips = sorted_tips.drain(TIPS_LIMIT..); // keep only first 3 heavier tips
            debug!("Dropping tips {} because they are not in the first 3 heavier tips", dropped_tips.map(|h| h.to_string()).collect::<Vec<String>>().join(", "));
        }

        // find the newest timestamp
        let mut timestamp = 0;
        for tip in sorted_tips.iter() {
            let tip_timestamp = storage.get_timestamp_for_block_hash(tip).await?;
            if tip_timestamp > timestamp {
                timestamp = tip_timestamp;
            }
        }

        // Check that our current timestamp is correct
        let current_timestamp = get_current_time_in_millis();
        if current_timestamp < timestamp {
            warn!("Current timestamp is less than the newest tip timestamp, using newest timestamp from tips");
        } else {
            timestamp = current_timestamp;
        }

        let height = blockdag::calculate_height_at_tips(storage, sorted_tips.iter()).await?;
        let block = BlockHeader::new(get_version_at_height(self.get_network(), height), height, timestamp, sorted_tips, extra_nonce, address, IndexSet::new());

        Ok(block)
    }

    // Get the mining block template for miners
    // This function is called when a miner request a new block template
    // We create a block candidate with selected TXs from mempool
    pub async fn get_block_template_for_storage(&self, storage: &S, address: PublicKey) -> Result<BlockHeader, BlockchainError> {
        let mut block = self.get_block_header_template_for_storage(storage, address).await?;

        trace!("Locking mempool for building block template");
        let mempool = self.mempool.read().await;
        trace!("Mempool locked for building block template");

        // use the mempool cache to get all availables txs grouped by account
        let caches = mempool.get_caches();

        // Build the tx selector using the mempool
        let mut tx_selector = TxSelector::with_capacity(caches.len());
        for cache in caches.values() {
            let cache_txs = cache.get_txs();
            let mut txs = Vec::with_capacity(cache_txs.len());
            // Map every tx hash to a TxSelectorEntry
            for tx_hash in cache_txs.iter() {
                let sorted_tx = mempool.get_sorted_tx(tx_hash)?;
                txs.push(TxSelectorEntry { size: sorted_tx.get_size(), hash: tx_hash, tx: sorted_tx.get_tx() });
            }
            tx_selector.push_group(txs);
        }

        // size of block
        let mut block_size = block.size();
        let mut total_txs_size = 0;

        // data used to verify txs
        let stable_topoheight = self.get_stable_topoheight();
        let stable_height = self.get_stable_height();
        let topoheight = self.get_topo_height();

        trace!("build chain state for block template");
        let mut chain_state = ChainState::new(storage, &self.environment, stable_topoheight, topoheight, block.get_version());

        if !tx_selector.is_empty() {
            let mut failed_sources = HashSet::new();
            // Search all txs that were processed in tips
            // This help us to determine if a TX was already included or not based on our DAG
            // Hopefully, this should never be triggered because the mempool is cleaned based on our state
            let processed_txs = self.get_all_txs_until_height(storage, stable_height, block.get_tips().iter().cloned(), false).await?;
            while let Some(TxSelectorEntry { size, hash, tx }) = tx_selector.next() {
                if block_size + total_txs_size + size >= MAX_BLOCK_SIZE || block.txs_hashes.len() >= u16::MAX as usize {
                    debug!("Stopping to include new TXs in this block, final size: {}, count: {}", human_bytes::human_bytes((block_size + total_txs_size) as f64), block.txs_hashes.len());
                    break;
                }

                // Check if the TX is already in the block
                if processed_txs.contains(hash) {
                    debug!("Skipping TX {} because it is already in the DAG branch", hash);
                    continue;
                }

                if !self.skip_block_template_txs_verification {
                    // Check if the TX is valid for this potential block
                    trace!("Checking TX {} with nonce {}, {}", hash, tx.get_nonce(), tx.get_source().as_address(self.network.is_mainnet()));

                    let source = tx.get_source();
                    if failed_sources.contains(&source) {
                        debug!("Skipping TX {} because its source has failed before", hash);
                        continue;
                    }

                    if let Err(e) = tx.verify(&hash, &mut chain_state).await {
                        warn!("TX {} ({}) is not valid for mining: {}", hash, source.as_address(self.network.is_mainnet()), e);
                        failed_sources.insert(source);
                        continue;
                    }
                }

                trace!("Selected {} (nonce: {}, fees: {}) for mining", hash, tx.get_nonce(), format_tos(tx.get_fee()));
                // TODO no clone
                block.txs_hashes.insert(hash.as_ref().clone());
                block_size += HASH_SIZE; // add the hash size
                total_txs_size += size;
            }
        }

        Ok(block)
    }

    // Build a block using the header and search for TXs in mempool and storage
    pub async fn build_block_from_header(&self, header: Immutable<BlockHeader>) -> Result<Block, BlockchainError> {
        trace!("Searching TXs for block at height {}", header.get_height());
        let mut transactions: Vec<Immutable<Transaction>> = Vec::with_capacity(header.get_txs_count());
        let storage = self.storage.read().await;
        let mempool = self.mempool.read().await;
        trace!("Mempool lock acquired for building block from header");
        for hash in header.get_txs_hashes() {
            trace!("Searching TX {} for building block", hash);
            // at this point, we don't want to lose/remove any tx, we clone it only
            let tx = if mempool.contains_tx(hash) {
                mempool.get_tx(hash)?
            } else {
                storage.get_transaction(hash).await?
            };

            transactions.push(Immutable::Arc(tx));
        }
        let block = Block::new(header, transactions);
        Ok(block)
    }

    // Add a new block in chain
    pub async fn add_new_block(&self, block: Block, block_hash: Option<Immutable<Hash>>, broadcast: BroadcastOption, mining: bool) -> Result<(), BlockchainError> {
        debug!("locking storage to add a new block in chain");
        let mut storage = self.storage.write().await;
        debug!("storage lock acquired for new block to add");
        self.add_new_block_for_storage(&mut storage, block, block_hash, broadcast, mining).await
    }

    // Add a new block in chain using the requested storage
    pub async fn add_new_block_for_storage(&self, storage: &mut S, block: Block, block_hash: Option<Immutable<Hash>>, broadcast: BroadcastOption, mining: bool) -> Result<(), BlockchainError> {
        let start = Instant::now();

        // Expected version for this block
        let version = get_version_at_height(self.get_network(), block.get_height());

        // Verify that the block is on the correct version
        if block.get_version() != version {
            return Err(BlockchainError::InvalidBlockVersion)
        }

        // Either check or use the precomputed one
        let block_hash = if let Some(hash) = block_hash {
            hash
        } else {
            Immutable::Owned(block.hash())
        };

        debug!("Add new block {}", block_hash);
        if storage.has_block_with_hash(&block_hash).await? {
            debug!("Block {} is already in chain!", block_hash);
            return Err(BlockchainError::AlreadyInChain)
        }
        debug!("Block {} is not in chain, processing it", block_hash);

        let current_timestamp = get_current_time_in_millis(); 
        if block.get_timestamp() > current_timestamp + TIMESTAMP_IN_FUTURE_LIMIT { // accept 2s in future
            debug!("Block timestamp is too much in future!");
            return Err(BlockchainError::TimestampIsInFuture(current_timestamp, block.get_timestamp()));
        }

        let tips_count = block.get_tips().len();
        debug!("Tips count for this new {}: {}", block, tips_count);
        // only 3 tips are allowed
        if tips_count > TIPS_LIMIT {
            debug!("Invalid tips count, got {} but maximum allowed is {}", tips_count, TIPS_LIMIT);
            return Err(BlockchainError::InvalidTipsCount(block_hash.into_owned(), tips_count))
        }

        let mut current_height = self.get_height();
        if tips_count == 0 && current_height != 0 {
            debug!("Expected at least one previous block for this block {}", block_hash);
            return Err(BlockchainError::ExpectedTips)
        }

        if tips_count > 0 && block.get_height() == 0 {
            debug!("Invalid block height, got height 0 but tips are present for this block {}", block_hash);
            return Err(BlockchainError::BlockHeightZeroNotAllowed)
        }

        if tips_count == 0 && block.get_height() != 0 {
            debug!("Invalid tips count, got {} but current height is {} with block height {}", tips_count, current_height, block.get_height());
            return Err(BlockchainError::InvalidTipsCount(block_hash.into_owned(), tips_count))
        }

        // block contains header and full TXs
        let block_size = block.size();
        if block_size > MAX_BLOCK_SIZE {
            debug!("Block size ({} bytes) is greater than the limit ({} bytes)", block.size(), MAX_BLOCK_SIZE);
            return Err(BlockchainError::InvalidBlockSize(MAX_BLOCK_SIZE, block.size()));
        }

        for tip in block.get_tips() {
            if !storage.has_block_with_hash(tip).await? {
                debug!("This block ({}) has a TIP ({}) which is not present in chain", block_hash, tip);
                return Err(BlockchainError::InvalidTipsNotFound(block_hash.into_owned(), tip.clone()))
            }
        }

        let block_height_by_tips = blockdag::calculate_height_at_tips(storage, block.get_tips().iter()).await?;
        if block_height_by_tips != block.get_height() {
            debug!("Invalid block height {}, expected {} for this block {}", block.get_height(), block_height_by_tips, block_hash);
            return Err(BlockchainError::InvalidBlockHeight(block_height_by_tips, block.get_height()))
        }

        let stable_height = self.get_stable_height();
        if tips_count > 0 {
            debug!("Height by tips: {}, stable height: {}", block_height_by_tips, stable_height);

            if block_height_by_tips < stable_height {
                debug!("Invalid block height by tips {} for this block ({}), its height is in stable height {}", block_height_by_tips, block_hash, stable_height);
                return Err(BlockchainError::InvalidBlockHeightStableHeight)
            }
        }

        // Verify the reachability of the block
        if !self.verify_non_reachability(storage, block.get_tips()).await? {
            debug!("{} with hash {} has an invalid reachability", block, block_hash);
            return Err(BlockchainError::InvalidReachability)
        }

        for hash in block.get_tips() {
            let previous_timestamp = storage.get_timestamp_for_block_hash(hash).await?;
            // block timestamp can't be less than previous block.
            if block.get_timestamp() < previous_timestamp {
                debug!("Invalid block timestamp, parent ({}) is less than new block {}", hash, block_hash);
                return Err(BlockchainError::TimestampIsLessThanParent(block.get_timestamp()));
            }

            trace!("calculate distance from mainchain for tips: {}", hash);

            // We're processing the block tips, so we can't use the block height as it may not be in the chain yet
            let height = block_height_by_tips.checked_sub(1).unwrap_or(0);
            if !self.verify_distance_from_mainchain(storage, hash, height).await? {
                error!("{} with hash {} have deviated too much (current height: {}, block height: {})", block, block_hash, current_height, block_height_by_tips);
                return Err(BlockchainError::BlockDeviation)
            }
        }

        if tips_count > 1 {
            let best_tip = blockdag::find_best_tip_by_cumulative_difficulty(storage, block.get_tips().iter()).await?;
            debug!("Best tip selected for this new block is {}", best_tip);
            for hash in block.get_tips() {
                if best_tip != hash {
                    if !self.validate_tips(storage, best_tip, hash).await? {
                        debug!("Tip {} is invalid, difficulty can't be less than 91% of {}", hash, best_tip);
                        return Err(BlockchainError::InvalidTipsDifficulty(block_hash.into_owned(), hash.clone()))
                    }
                }
            }
        }

        // verify PoW and get difficulty for this block based on tips
        let skip_pow = self.skip_pow_verification();
        let pow_hash = if skip_pow {
            // Simulator is enabled, we don't need to compute the PoW hash
            Hash::zero()
        } else {
            let algorithm = get_pow_algorithm_for_version(version);
            block.get_pow_hash(algorithm)?
        };
        debug!("POW hash: {}, skipped: {}", pow_hash, skip_pow);
        let (difficulty, p) = self.verify_proof_of_work(storage, &pow_hash, block.get_tips().iter()).await?;
        debug!("PoW is valid for difficulty {}", difficulty);

        let mut current_topoheight = self.get_topo_height();
        // Transaction verification
        // Here we are going to verify all TXs in the block
        // For this, we must select TXs that are not doing collisions with other TXs in block
        // TX already added in the same DAG branch (block tips) are rejected because miner should be aware of it
        // TXs that are already executed in stable height are also rejected whatever DAG branch it is
        // If the TX is executed by another branch, we skip the verification because DAG will choose which branch will execute the TX
        {
            let hashes_len = block.get_txs_hashes().len();
            let txs_len = block.get_transactions().len();
            if  hashes_len != txs_len {
                debug!("Block {} has an invalid block header, transactions count mismatch (expected {} got {})!", block_hash, txs_len, hashes_len);
                return Err(BlockchainError::InvalidBlockTxs(hashes_len, txs_len));
            }

            // Serializer support only up to u16::MAX txs per block
            let limit = u16::MAX as usize;
            if txs_len > limit {
                debug!("Block {} has an invalid block header, transactions count is bigger than limit (expected max {} got {})!", block_hash, limit, hashes_len);
                return Err(BlockchainError::InvalidBlockTxs(limit, txs_len));
            }

            trace!("verifying {} TXs in block {}", txs_len, block_hash);
            // Cache to retrieve only one time all TXs hashes until stable height
            let mut all_parents_txs: Option<HashSet<Hash>> = None;

            // All transactions to be verified in one batch
            let mut txs_batch = Vec::with_capacity(block.get_txs_count());
            // All transactions grouped per source key
            // used for multi threading
            let mut txs_grouped = HashMap::new();
            let mut total_outputs = 0;
            let is_v2_enabled = version >= BlockVersion::V2;
            for (tx, hash) in block.get_transactions().iter().zip(block.get_txs_hashes()) {
                let tx_size = tx.size();
                if tx_size > MAX_TRANSACTION_SIZE {
                    return Err(BlockchainError::TxTooBig(tx_size, MAX_TRANSACTION_SIZE))
                }

                // verification that the real TX Hash is the same as in block header (and also check the correct order)
                let tx_hash = tx.hash();
                if tx_hash != *hash {
                    debug!("Invalid tx {} vs {} in block header", tx_hash, hash);
                    return Err(BlockchainError::InvalidTxInBlock(tx_hash))
                }

                debug!("Verifying TX {}", tx_hash);
                // check that the TX included is not executed in stable height
                let is_executed = storage.is_tx_executed_in_a_block(hash)?;
                if is_executed {
                    let block_executor = storage.get_block_executor_for_tx(hash)?;
                    debug!("Tx {} was executed in {}", hash, block_executor);
                    let block_executor_height = storage.get_height_for_block_hash(&block_executor).await?;
                    // if the tx was executed below stable height, reject whole block!
                    if block_executor_height <= stable_height {
                        debug!("Block {} contains a dead tx {} from stable height {}", block_hash, tx_hash, stable_height);
                        return Err(BlockchainError::DeadTxFromStableHeight(block_hash.into_owned(), tx_hash, stable_height, block_executor))
                    }
                }

                // If the TX is already executed,
                // we should check that the TX is not in block tips
                // For v2 and above, all TXs that are presents in block TIPs are rejected
                if is_v2_enabled || (is_executed && !is_v2_enabled) {
                    // now we should check that the TX was not executed in our TIP branch
                    // because that mean the miner was aware of the TX execution and still include it
                    if all_parents_txs.is_none() {
                        debug!("Loading all TXs until height {} for block {} (executed only: {})", stable_height, block_hash, !is_v2_enabled);
                        let txs = self.get_all_txs_until_height(
                            storage,
                            stable_height,
                            block.get_tips().iter().cloned(),
                            !is_v2_enabled
                        ).await?;
                        all_parents_txs = Some(txs);
                    }

                    // if its the case, we should reject the block
                    if let Some(txs) = all_parents_txs.as_ref() {
                        // miner knows this tx was already executed because its present in block tips
                        // reject the whole block
                        if txs.contains(&tx_hash) {
                            debug!("Malicious Block {} formed, contains a dead tx {}, is executed: {}", block_hash, tx_hash, is_executed);
                            return Err(BlockchainError::DeadTxFromTips(block_hash.into_owned(), tx_hash))
                        } else if is_executed {
                            // otherwise, all looks good but because the TX was executed in another branch, we skip verification
                            // DAG will choose which branch will execute the TX
                            debug!("TX {} was executed in another branch, skipping verification", tx_hash);
    
                            // because TX was already validated & executed and is not in block tips
                            // we can safely skip the verification of this TX
                            continue;
                        }
                    }
                }

                total_outputs += tx.get_outputs_count();
                txs_batch.push((tx, hash));
                txs_grouped.entry(tx.get_source())
                    .or_insert_with(Vec::new)
                    .push((tx, hash));
            }

            if !txs_batch.is_empty() {
                debug!("proof verifications of {} TXs from {} sources with {} outputs in block {}", txs_batch.len(), txs_grouped.len(), total_outputs, block_hash);
                // Track how much time it takes to verify them all
                let start = Instant::now();
                // If multi thread is enabled and we have more than one source
                // Otherwise its not worth-it to move it on another thread
                if self.txs_verification_threads_count > 1 && is_multi_threads_supported() {
                    let mut batches_count = txs_grouped.len();
                    if batches_count > self.txs_verification_threads_count {
                        debug!("Batches count ({}) is above configured threads ({}), capping it", batches_count, self.txs_verification_threads_count);
                        batches_count = self.txs_verification_threads_count;
                    }

                    debug!("using multi-threading mode to verify the transactions in {} batches", batches_count);
                    let mut batches = vec![Vec::new(); batches_count];
                    let mut queue: VecDeque<_> = txs_grouped.into_values().collect();

                    let mut i = 0;
                    // TODO: load balance more!
                    while let Some(group) = queue.pop_front() {
                        batches[i % batches_count].extend(group);
                        i += 1;
                    }

                    // Channel to be notified of any batch failing
                    let (sender, _) = broadcast::channel::<()>(1);
                    let (_, results) = async_scoped::TokioScope::scope_and_block(|scope| {
                        let storage = &*storage;
                        let stable_topoheight = self.get_stable_topoheight();
                        for (i, batch) in batches.into_iter().enumerate() {
                            let sender = &sender;
                            let mut receiver = sender.subscribe();

                            scope.spawn(async move {
                                let mut chain_state = ChainState::new(storage, &self.environment, stable_topoheight, current_topoheight, version);
                                tokio::select! {
                                    res = Transaction::verify_batch(batch.as_slice(), &mut chain_state) => {
                                        if let Err(e) = &res {
                                            if sender.send(()).is_err() {
                                                error!("Error while notifying others tasks about batch #{} failing with error: {}", i, e);
                                            }
                                        }

                                        res
                                    },
                                    _ = receiver.recv() => {
                                        info!("Exiting batch task #{} due to exit signal received", i);
                                        Ok(())
                                    }
                                }
                            });
                        }
                    });

                    for (i, result) in results.into_iter().enumerate() {
                        match result {
                            Ok(Ok(())) => {},
                            Ok(Err(e)) => {
                                error!("Error on batch #{}: {}", i, e);
                                return Err(e.into());
                            },
                            Err(e) => {
                                error!("Error while joining batch task #{}: {} ", i, e);
                                return Err(BlockchainError::InvalidTransactionMultiThread)
                            }
                        };
                    }
                } else {
                    // Verify all valid transactions in one batch
                    let mut chain_state = ChainState::new(storage, &self.environment, self.get_stable_topoheight(), current_topoheight, version);
                    Transaction::verify_batch(txs_batch.as_slice(), &mut chain_state).await?;
                }
                debug!("Verified {} transactions in {}ms", txs_batch.len(), start.elapsed().as_millis());
            }
        }

        // Save transactions & block
        let (block, txs) = block.split();
        let block = block.to_arc();

        debug!("Saving block {} on disk", block_hash);
        // Add block to chain
        // TODO: best would be to not clone block hash
        storage.save_block(block.clone(), &txs, difficulty, p, block_hash.to_owned()).await?;
        storage.add_block_execution_to_order(&block_hash).await?;

        let block_hash = block_hash.to_arc();
        debug!("Block {} saved on disk, compute cumulative difficulty", block_hash);

        // Compute cumulative difficulty for block
        // We retrieve it to pass it as a param below for p2p broadcast
        let cumulative_difficulty = {
            let cumulative_difficulty: CumulativeDifficulty = if tips_count == 0 {
                GENESIS_BLOCK_DIFFICULTY.into()
            } else {
                debug!("Computing cumulative difficulty for block {}", block_hash);
                let (base, base_height) = self.find_common_base(storage, block.get_tips()).await?;
                debug!("Common base found: {}, height: {}", base, base_height);
                let (_, cumulative_difficulty) = self.find_tip_work_score(storage, &block_hash, &base, base_height).await?;
                cumulative_difficulty
            };
            storage.set_cumulative_difficulty_for_block_hash(&block_hash, cumulative_difficulty).await?;
            debug!("Cumulative difficulty for block {}: {}", block_hash, cumulative_difficulty);
            cumulative_difficulty
        };

        // Broadcast to p2p nodes the block asap as its valid
        if broadcast.p2p() {
            debug!("Broadcasting block");
            if let Some(p2p) = self.p2p.read().await.as_ref() {
                trace!("P2p locked, broadcasting in new task");
                let p2p = p2p.clone();
                let pruned_topoheight = storage.get_pruned_topoheight().await?;
                let block = block.clone();
                let block_hash = block_hash.clone();
                spawn_task("broadcast-block", async move {
                    p2p.broadcast_block(
                        &block,
                        cumulative_difficulty,
                        current_topoheight,
                        current_height.max(block.get_height()),
                        pruned_topoheight,
                        block_hash,
                        mining
                    ).await;
                });
            }
        } else {
            debug!("Not broadcasting block {} because broadcast is disabled", block_hash);
        }

        let mut tips = storage.get_tips().await?;
        // TODO: best would be to not clone
        tips.insert(block_hash.as_ref().clone());
        for hash in block.get_tips() {
            tips.remove(hash);
        }
        debug!("New tips: {}", tips.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));

        let (base_hash, base_height) = self.find_common_base(storage, &tips).await?;
        debug!("New base hash: {}, height: {}", base_hash, base_height);
        let best_tip = self.find_best_tip(storage, &tips, &base_hash, base_height).await?;
        debug!("Best tip selected: {}", best_tip);

        let base_topo_height = storage.get_topo_height_for_hash(&base_hash).await?;
        // generate a full order until base_topo_height
        let mut full_order = self.generate_full_order(storage, &best_tip, &base_hash, base_height, base_topo_height).await?;
        debug!("Generated full order size: {}, with base ({}) topo height: {}", full_order.len(), base_hash, base_topo_height);
        trace!("Full order: {}", full_order.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "));

        // rpc server lock
        let rpc_server = self.rpc.read().await;
        let should_track_events = if let Some(rpc) = rpc_server.as_ref() {
            rpc.get_tracked_events().await
        } else {
            HashSet::new()
        };

        // track all events to notify websocket
        let mut events: HashMap<NotifyEvent, Vec<Value>> = HashMap::new();
        // Track all orphaned transactions
        // We keep in order all orphaned txs to try to re-add them in the mempool
        let mut orphaned_transactions = IndexSet::new();

        // order the DAG (up to TOP_HEIGHT - STABLE_LIMIT)
        let mut highest_topo = 0;
        // Tells if the new block added is ordered in DAG or not
        let block_is_ordered = full_order.contains(block_hash.as_ref());
        {
            let mut is_written = base_topo_height == 0;
            let mut skipped = 0;
            // detect which part of DAG reorg stay, for other part, undo all executed txs
            debug!("Detecting stable point of DAG and cleaning txs above it");
            {
                let mut topoheight = base_topo_height;
                while topoheight <= current_topoheight {
                    let hash_at_topo = storage.get_hash_at_topo_height(topoheight).await?;
                    trace!("Cleaning txs at topoheight {} ({})", topoheight, hash_at_topo);
                    if !is_written {
                        if let Some(order) = full_order.first() {
                            // Verify that the block is still at the same topoheight
                            if storage.is_block_topological_ordered(order).await && *order == hash_at_topo {
                                trace!("Hash {} at topo {} stay the same, skipping cleaning", hash_at_topo, topoheight);
                                // remove the hash from the order because we don't need to recompute it
                                full_order.shift_remove_index(0);
                                topoheight += 1;
                                skipped += 1;
                                continue;
                            }
                        }
                        // if we are here, it means that the block was re-ordered
                        is_written = true;
                    }

                    debug!("Cleaning transactions executions at topo height {} (block {})", topoheight, hash_at_topo);

                    let block = storage.get_block_header_by_hash(&hash_at_topo).await?;

                    // Block may be orphaned if its not in the new full order set
                    let is_orphaned = !full_order.contains(&hash_at_topo);
                    // Notify if necessary that we have a block orphaned
                    if is_orphaned && should_track_events.contains(&NotifyEvent::BlockOrphaned) {
                        let value = json!(BlockOrphanedEvent {
                            block_hash: Cow::Borrowed(&hash_at_topo),
                            old_topoheight: topoheight,
                        });
                        events.entry(NotifyEvent::BlockOrphaned).or_insert_with(Vec::new).push(value);
                    }

                    // mark txs as unexecuted if it was executed in this block
                    for tx_hash in block.get_txs_hashes() {
                        if storage.is_tx_executed_in_block(tx_hash, &hash_at_topo)? {
                            debug!("Removing execution of {}", tx_hash);
                            storage.remove_tx_executed(tx_hash)?;
                            storage.delete_contract_outputs_for_tx(tx_hash).await?;

                            if is_orphaned {
                                debug!("Tx {} is now marked as orphaned", tx_hash);
                                orphaned_transactions.insert(tx_hash.clone());
                            }
                        }
                    }

                    // Delete changes made by this block
                    storage.delete_versioned_data_at_topoheight(topoheight).await?;

                    topoheight += 1;
                }

                // Only clear the versioned data caches if we delete any data
                if is_written {
                    storage.clear_versioned_data_caches().await?;
                }
            }

            // This is used to verify that each nonce is used only one time
            let mut nonce_checker = NonceChecker::new();
            // Side blocks counter per height
            let mut side_blocks: HashMap<u64, u64> = HashMap::new();
            // time to order the DAG that is moving
            debug!("Ordering blocks based on generated DAG order ({} blocks)", full_order.len());
            for (i, hash) in full_order.into_iter().enumerate() {
                highest_topo = base_topo_height + skipped + i as u64;

                // if block is not re-ordered and it's not genesis block
                // because we don't need to recompute everything as it's still good in chain
                if !is_written && tips_count != 0 && storage.is_block_topological_ordered(&hash).await && storage.get_topo_height_for_hash(&hash).await? == highest_topo {
                    trace!("Block ordered {} stay at topoheight {}. Skipping...", hash, highest_topo);
                    continue;
                }
                is_written = true;

                trace!("Ordering block {} at topoheight {}", hash, highest_topo);

                storage.set_topo_height_for_block(&hash, highest_topo).await?;
                let past_supply = if highest_topo == 0 {
                    0
                } else {
                    storage.get_supply_at_topo_height(highest_topo - 1).await?
                };
                let past_burned_supply = if highest_topo == 0 {
                    0
                } else {
                    storage.get_burned_supply_at_topo_height(highest_topo - 1).await?
                };

                // Block for this hash
                let block = storage.get_block_by_hash(&hash).await?;

                // Reward the miner of this block
                // We have a decreasing block reward if there is too much side block
                let is_side_block = self.is_side_block_internal(storage, &hash, highest_topo).await?;
                let height = block.get_height();
                let side_blocks_count = match side_blocks.entry(height) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let mut count = 0;
                        let blocks_at_height = storage.get_blocks_at_height(height).await?;
                        for block in blocks_at_height {
                            if block != hash && self.is_side_block_internal(storage, &block, highest_topo).await? {
                                count += 1;
                                debug!("Found side block {} at height {}", block, height);
                            }
                        }

                        entry.insert(count)
                    },
                };

                let mut block_reward = self.internal_get_block_reward(past_supply, is_side_block, *side_blocks_count).await?;
                trace!("set block {} reward to {} at {} (height {}, side block: {}, {} {}%)", hash, block_reward, highest_topo, height, is_side_block, side_blocks_count, side_block_reward_percentage(*side_blocks_count));
                if is_side_block {
                    *side_blocks_count += 1;
                }

                storage.set_block_reward_at_topo_height(highest_topo, block_reward)?;
                let supply = past_supply + block_reward;
                trace!("set block supply to {} at {}", supply, highest_topo);
                storage.set_supply_at_topo_height(highest_topo, supply)?;

                // All fees from the transactions executed in this block
                let mut total_fees = 0;
                // Chain State used for the verification
                trace!("building chain state to execute TXs in block {}", block_hash);
                let mut chain_state = ApplicableChainState::new(
                    storage,
                    &self.environment,
                    base_topo_height,
                    highest_topo,
                    version,
                    past_burned_supply,
                    &hash,
                    &block,
                );

                // compute rewards & execute txs
                for (tx, tx_hash) in block.get_transactions().iter().zip(block.get_txs_hashes()) { // execute all txs
                    // Link the transaction hash to this block
                    if !chain_state.get_mut_storage().add_block_linked_to_tx_if_not_present(&tx_hash, &hash)? {
                        trace!("Block {} is now linked to tx {}", hash, tx_hash);
                    }

                    // check that the tx was not yet executed in another tip branch
                    if chain_state.get_storage().is_tx_executed_in_a_block(tx_hash)? {
                        trace!("Tx {} was already executed in a previous block, skipping...", tx_hash);
                    } else {
                        // tx was not executed, but lets check that it is not a potential double spending
                        // check that the nonce is not already used
                        if !nonce_checker.use_nonce(chain_state.get_storage(), tx.get_source(), tx.get_nonce(), highest_topo).await? {
                            warn!("Malicious TX {}, it is a potential double spending with same nonce {}, skipping...", tx_hash, tx.get_nonce());
                            // TX will be orphaned
                            orphaned_transactions.insert(tx_hash.clone());
                            continue;
                        }

                        // Execute the transaction by applying changes in storage
                        debug!("Executing tx {} in block {} with nonce {}", tx_hash, hash, tx.get_nonce());
                        if let Err(e) = tx.apply_with_partial_verify(tx_hash, &mut chain_state).await {
                            warn!("Error while executing TX {} with current DAG org: {}", tx_hash, e);
                            // TX may be orphaned if not added again in good order in next blocks
                            orphaned_transactions.insert(tx_hash.clone());
                            continue;
                        }

                        // Calculate the new nonce
                        // This has to be done in case of side blocks where TX B would be before TX A
                        let next_nonce = nonce_checker.get_new_nonce(tx.get_source(), self.network.is_mainnet())?;
                        chain_state.as_mut().update_account_nonce(tx.get_source(), next_nonce).await?;

                        // mark tx as executed
                        chain_state.get_mut_storage().set_tx_executed_in_block(tx_hash, &hash)?;

                        // Delete the transaction from  the list if it was marked as orphaned
                        if orphaned_transactions.shift_remove(tx_hash) {
                            trace!("Transaction {} was marked as orphaned, but got executed again", tx_hash);
                        }

                        // if the rpc_server is enable, track events
                        if should_track_events.contains(&NotifyEvent::TransactionExecuted) {
                            let value = json!(TransactionExecutedEvent {
                                tx_hash: Cow::Borrowed(&tx_hash),
                                block_hash: Cow::Borrowed(&hash),
                                topoheight: highest_topo,
                            });
                            events.entry(NotifyEvent::TransactionExecuted).or_insert_with(Vec::new).push(value);
                        }

                        match tx.get_data() {
                            TransactionType::InvokeContract(payload) => {
                                let event = NotifyEvent::InvokeContract {
                                    contract: payload.contract.clone(),
                                };

                                if should_track_events.contains(&event) {
                                    let is_mainnet = self.network.is_mainnet();

                                    if let Some(contract_outputs) = chain_state.get_contract_outputs_for_tx(&tx_hash) {
                                        let contract_outputs = contract_outputs.into_iter()
                                        .map(|output| RPCContractOutput::from_output(output, is_mainnet))
                                        .collect::<Vec<_>>();

                                        let value = json!(InvokeContractEvent {
                                            tx_hash: Cow::Borrowed(&tx_hash),
                                            block_hash: Cow::Borrowed(&hash),
                                            topoheight: highest_topo,
                                            contract_outputs,
                                        });
                                        events.entry(event).or_insert_with(Vec::new).push(value);
                                    }
                                }
                            },
                            TransactionType::DeployContract(_) => {
                                if should_track_events.contains(&NotifyEvent::DeployContract) {
                                    let value = json!(NewContractEvent {
                                        contract: Cow::Borrowed(&tx_hash),
                                        block_hash: Cow::Borrowed(&hash),
                                        topoheight: highest_topo,
                                    });
                                    events.entry(NotifyEvent::DeployContract).or_insert_with(Vec::new).push(value);
                                }
                            }
                            _ => {}
                        }

                        // Increase total tx fees for miner
                        total_fees += tx.get_fee();
                    }
                }

                let dev_fee_percentage = get_block_dev_fee(block.get_height());
                // Dev fee are only applied on block reward
                // Transaction fees are not affected by dev fee
                if dev_fee_percentage != 0 {
                    let dev_fee_part = block_reward * dev_fee_percentage / 100;
                    chain_state.reward_miner(&DEV_PUBLIC_KEY, dev_fee_part).await?;
                    block_reward -= dev_fee_part;    
                }

                // reward the miner
                // Miner gets the block reward + total fees + gas fee
                let gas_fee = chain_state.get_gas_fee();
                chain_state.reward_miner(block.get_miner(), block_reward + total_fees + gas_fee).await?;

                // Fire all the contract events
                {
                    let start = Instant::now();
                    let contract_tracker = chain_state.get_contract_tracker();
                    let is_mainnet = self.network.is_mainnet();

                    // We want to only fire one event per key/hash pair
                    if should_track_events.contains(&NotifyEvent::NewAsset) {
                        let entry = events.entry(NotifyEvent::NewAsset)
                            .or_insert_with(Vec::new);

                        for asset in contract_tracker.assets_created.iter() {
                            let value = json!(NewAssetEvent {
                                asset: Cow::Borrowed(asset),
                                block_hash: Cow::Borrowed(&hash),
                                topoheight: highest_topo,
                            });

                            entry.push(value);
                        }
                    }

                    for (key, assets) in contract_tracker.transfers.iter() {
                        let event = NotifyEvent::ContractTransfer { address: key.as_address(is_mainnet) };
                        if should_track_events.contains(&event) {
                            let entry = events.entry(event)
                                .or_insert_with(Vec::new);

                            for (asset, amount) in assets {
                                let value = json!(ContractTransferEvent {
                                    asset: Cow::Borrowed(asset),
                                    amount: *amount,
                                    block_hash: Cow::Borrowed(&hash),
                                    topoheight: highest_topo,
                                });
                                
                                entry.push(value);
                            }
                        }
                    }

                    let caches = chain_state.get_contracts_cache();
                    for (contract, cache) in caches {
                        for (id, elements) in cache.events.iter() {
                            let event = NotifyEvent::ContractEvent {
                                contract: (*contract).clone(),
                                id: *id
                            };

                            if should_track_events.contains(&event) {
                                let entry = events.entry(event)
                                    .or_insert_with(Vec::new);

                                for el in elements {
                                    entry.push(json!(ContractEvent {
                                        data: Cow::Borrowed(el)
                                    }));
                                }
                            }
                        }
                    }

                    debug!("Processed contracts events in {}ms", start.elapsed().as_millis());
                }

                // apply changes from Chain State
                chain_state.apply_changes().await?;

                if should_track_events.contains(&NotifyEvent::BlockOrdered) {
                    let value = json!(BlockOrderedEvent {
                        block_hash: Cow::Borrowed(&hash),
                        block_type: get_block_type_for_block(self, storage, &hash).await.unwrap_or(BlockType::Normal),
                        topoheight: highest_topo,
                    });
                    events.entry(NotifyEvent::BlockOrdered).or_insert_with(Vec::new).push(value);
                }
            }
        }

        let best_height = storage.get_height_for_block_hash(best_tip).await?;
        let mut new_tips = Vec::new();
        for hash in tips {
            if self.verify_distance_from_mainchain(storage, &hash, current_height).await? {
                trace!("Adding {} as new tips", hash);
                new_tips.push(hash);
            } else {
                warn!("Rusty TIP declared stale {} with best height: {}", hash, best_height);
            }
        }

        tips = HashSet::new();
        debug!("find best tip by cumulative difficulty");
        let best_tip = blockdag::find_best_tip_by_cumulative_difficulty(storage, new_tips.iter()).await?.clone();
        for hash in new_tips {
            if best_tip != hash {
                if !self.validate_tips(storage, &best_tip, &hash).await? {
                    warn!("Rusty TIP {} declared stale", hash);
                } else {
                    debug!("Tip {} is valid, adding to final Tips list", hash);
                    tips.insert(hash);
                }
            }
        }
        tips.insert(best_tip);

        // save highest topo height
        debug!("Highest topo height found: {}", highest_topo);
        let extended = highest_topo > current_topoheight;
        if current_height == 0 || extended {
            debug!("Blockchain height extended, current topoheight is now {} (previous was {})", highest_topo, current_topoheight);
            storage.set_top_topoheight(highest_topo)?;
            self.topoheight.store(highest_topo, Ordering::Release);
            current_topoheight = highest_topo;
        }

        // If block is directly orphaned
        // Mark all TXs ourself as linked to it
        if !block_is_ordered {
            trace!("Block {} is orphaned, marking all TXs as linked to it", block_hash);
            for tx_hash in block.get_txs_hashes() {
                storage.add_block_linked_to_tx_if_not_present(&tx_hash, &block_hash)?;
            }
        }

        // auto prune mode
        if extended {
            if let Some(keep_only) = self.auto_prune_keep_n_blocks {
                // check that the topoheight is greater than the safety limit
                // and that we can prune the chain using the config while respecting the safety limit
                if current_topoheight % keep_only == 0 && current_topoheight - keep_only > 0 {
                    info!("Auto pruning chain until topoheight {} (keep only {} blocks)", current_topoheight - keep_only, keep_only);
                    let start = Instant::now();
                    if let Err(e) = self.prune_until_topoheight_for_storage(current_topoheight - keep_only, storage).await {
                        warn!("Error while trying to auto prune chain: {}", e);
                    }

                    info!("Auto pruning done in {}ms", start.elapsed().as_millis());
                }
            }
        }

        // Store the new tips available
        storage.store_tips(&tips)?;

        if current_height == 0 || block.get_height() > current_height {
            debug!("storing new top height {}", block.get_height());
            storage.set_top_height(block.get_height())?;
            self.height.store(block.get_height(), Ordering::Release);
            current_height = block.get_height();
        }

        // update stable height and difficulty in cache
        {
            if should_track_events.contains(&NotifyEvent::StableHeightChanged) {
                // detect the change in stable height
                let previous_stable_height = self.get_stable_height();
                if base_height != previous_stable_height {
                    let value = json!(StableHeightChangedEvent {
                        previous_stable_height,
                        new_stable_height: base_height
                    });
                    events.entry(NotifyEvent::StableHeightChanged).or_insert_with(Vec::new).push(value);
                }
            }

            if should_track_events.contains(&NotifyEvent::StableTopoHeightChanged) {
                // detect the change in stable topoheight
                let previous_stable_topoheight = self.get_stable_topoheight();
                if base_topo_height != previous_stable_topoheight {
                    let value = json!(StableTopoHeightChangedEvent {
                        previous_stable_topoheight,
                        new_stable_topoheight: base_topo_height
                    });
                    events.entry(NotifyEvent::StableTopoHeightChanged).or_insert_with(Vec::new).push(value);
                }
            }

            // Update caches
            self.stable_height.store(base_height, Ordering::SeqCst);
            self.stable_topoheight.store(base_topo_height, Ordering::SeqCst);

            trace!("update difficulty in cache");
            let (difficulty, _) = self.get_difficulty_at_tips(storage, tips.iter()).await?;
            self.set_difficulty(difficulty).await;
        }

        // Check if the event is tracked
        let orphan_event_tracked = should_track_events.contains(&NotifyEvent::TransactionOrphaned);

        // Clean mempool from old txs if the DAG has been updated
        let mempool_deleted_txs = if highest_topo >= current_topoheight {
            debug!("Locking mempool write mode");
            let mut mempool = self.mempool.write().await;
            debug!("mempool write mode ok");
            let version = get_version_at_height(self.get_network(), current_height);
            mempool.clean_up(&*storage, &self.environment, base_topo_height, highest_topo, version).await
        } else {
            Vec::new()
        };

        if orphan_event_tracked {
            for (tx_hash, sorted_tx) in mempool_deleted_txs {
                // Delete it from our orphaned transactions list
                // This save some performances as it will not try to add it back and
                // consume resources for verifying the ZK Proof if we already know the answer
                if orphaned_transactions.shift_remove(tx_hash.as_ref()) {
                    debug!("Transaction {} was marked as orphaned, but got deleted from mempool. Prevent adding it back", tx_hash);
                }

                // Verify that the TX was not executed in a block
                if storage.is_tx_executed_in_a_block(&tx_hash)? {
                    trace!("Transaction {} was executed in a block, skipping orphaned event", tx_hash);
                    continue;
                }

                let data = RPCTransaction::from_tx(&sorted_tx.get_tx(), &tx_hash, storage.is_mainnet());
                let data = TransactionResponse {
                    blocks: None,
                    executed_in_block: None,
                    in_mempool: false,
                    first_seen: Some(sorted_tx.get_first_seen()),
                    data,
                };
                events.entry(NotifyEvent::TransactionOrphaned).or_insert_with(Vec::new).push(json!(data));
            }
        }

        // Now we can try to add back all transactions
        for tx_hash in orphaned_transactions {
            debug!("Trying to add orphaned tx {} back in mempool", tx_hash);
            // It is verified in add_tx_to_mempool function too
            // But to prevent loading the TX from storage and to fire wrong event
            if !storage.is_tx_executed_in_a_block(&tx_hash)? {
                let tx = match storage.get_transaction(&tx_hash).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        warn!("Error while loading orphaned tx: {}", e);
                        continue;
                    }
                };

                if let Err(e) = self.add_tx_to_mempool_with_storage_and_hash(storage, tx.clone(), Immutable::Owned(tx_hash.clone()), false).await {
                    warn!("Error while adding back orphaned tx {}: {}", tx_hash, e);
                    if !orphan_event_tracked {
                        // We couldn't add it back to mempool, let's notify this event
                        let data = RPCTransaction::from_tx(&tx, &tx_hash, storage.is_mainnet());
                        let data = TransactionResponse {
                            blocks: None,
                            executed_in_block: None,
                            in_mempool: false,
                            first_seen: None,
                            data,
                        };
                        events.entry(NotifyEvent::TransactionOrphaned).or_insert_with(Vec::new).push(json!(data));
                    }
                }
            }
        }

        // Flush to the disk
        if self.force_db_flush {
            storage.flush().await?;
        }

        info!("Processed block {} at height {} in {}ms with {} txs (DAG: {})", block_hash, block.get_height(), start.elapsed().as_millis(), block.get_txs_count(), block_is_ordered);

        if let Some(p2p) = self.p2p.read().await.as_ref().filter(|_| broadcast.p2p()) {
            trace!("P2p locked, ping peers");
            let p2p = p2p.clone();
            spawn_task("notify-ping-peers", async move {
                p2p.ping_peers().await;
            });
        }

        // broadcast to websocket new block
        if let Some(rpc) = rpc_server.as_ref() {
            // if we have a getwork server, and that its not from syncing, notify miners
            if broadcast.miners() {
                if let Some(getwork) = rpc.getwork_server() {
                    let getwork = getwork.clone();
                    spawn_task("notify-new-job", async move {
                        if let Err(e) = getwork.get_handler().notify_new_job().await {
                            debug!("Error while notifying new job to miners: {}", e);
                        }
                    });
                }
            }

            // atm, we always notify websocket clients
            trace!("Notifying websocket clients");
            if should_track_events.contains(&NotifyEvent::NewBlock) {
                match get_block_response(self, storage, &block_hash, &Block::new(Immutable::Arc(block), txs), block_size).await {
                    Ok(response) => {
                        events.entry(NotifyEvent::NewBlock).or_insert_with(Vec::new).push(response);
                    },
                    Err(e) => {
                        debug!("Error while getting block response for websocket: {}", e);
                    }
                };
            }

            let rpc = rpc.clone();
            // don't block mutex/lock more than necessary, we move it in another task
            spawn_task("rpc-notify-events", async move {
                for (event, values) in events {
                    for value in values {
                        if let Err(e) = rpc.notify_clients(&event, value).await {
                            debug!("Error while broadcasting event to websocket: {}", e);
                        }
                    }
                }
            });
        }

        Ok(())
    }

    // Get block reward based on the type of the block
    // Block shouldn't be orphaned
    pub async fn internal_get_block_reward(&self, past_supply: u64, is_side_block: bool, side_blocks_count: u64) -> Result<u64, BlockchainError> {
        trace!("internal get block reward");
        let block_reward = if is_side_block {
            let reward = get_block_reward(past_supply);
            let side_block_percent = side_block_reward_percentage(side_blocks_count);
            trace!("side block reward: {}%", side_block_percent);

            reward * side_block_percent / 100
        } else {
            get_block_reward(past_supply)
        };
        Ok(block_reward)
    }

    // Get the block reward for a block
    // This will search all blocks at same height and verify which one are side blocks
    pub async fn get_block_reward<P: DifficultyProvider + DagOrderProvider + BlocksAtHeightProvider>(&self, provider: &P, hash: &Hash, past_supply: u64, current_topoheight: TopoHeight) -> Result<u64, BlockchainError> {
        let is_side_block = self.is_side_block(provider, hash).await?;
        let mut side_blocks_count = 0;
        if is_side_block {
            // get the block height for this hash
            let height = provider.get_height_for_block_hash(hash).await?;
            let blocks_at_height = provider.get_blocks_at_height(height).await?;
            for block in blocks_at_height {
                if *hash != block && self.is_side_block_internal(provider, &block, current_topoheight).await? {
                    side_blocks_count += 1;
                }
            }
        }

        self.internal_get_block_reward(past_supply, is_side_block, side_blocks_count).await
    }

    // retrieve all txs hashes until height or until genesis block
    // for this we get all tips and recursively retrieve all txs from tips until we reach height
    async fn get_all_txs_until_height<P>(&self, provider: &P, until_height: u64, tips: impl Iterator<Item = Hash>, executed_only: bool) -> Result<HashSet<Hash>, BlockchainError>
    where
        P: DifficultyProvider + ClientProtocolProvider
    {
        trace!("get all txs until height {}", until_height);
        // All transactions hashes found under the stable height
        let mut hashes = HashSet::new();
        // Current queue of blocks to process
        let mut queue = IndexSet::new();
        // All already processed blocks
        let mut processed = IndexSet::new();
        queue.extend(tips);

        // get last element from queue (order doesn't matter and its faster than moving all elements)
        while let Some(hash) = queue.pop() {
            let block = provider.get_block_header_by_hash(&hash).await?;

            // check that the block height is higher than the height passed in param
            if block.get_height() >= until_height {
                // add all txs from block
                for tx in block.get_txs_hashes() {
                    // Check that we don't have it yet
                    if !hashes.contains(tx) {
                        // Then check that it's executed in this block
                        if !executed_only || (executed_only && provider.is_tx_executed_in_block(tx, &hash)?) {
                            // add it to the list
                            hashes.insert(tx.clone());
                        }
                    }
                }

                // add all tips from block (but check that we didn't already added it)
                for tip in block.get_tips() {
                    if !processed.contains(tip) {
                        processed.insert(tip.clone());
                        queue.insert(tip.clone());
                    }
                }
            }
        }

        Ok(hashes)
    }

    // if a block is not ordered, it's an orphaned block and its transactions are not honoured
    pub async fn is_block_orphaned_for_storage<P: DagOrderProvider>(&self, provider: &P, hash: &Hash) -> bool {
        trace!("is block {} orphaned", hash);
        !provider.is_block_topological_ordered(hash).await
    }

    pub async fn is_side_block<P: DifficultyProvider + DagOrderProvider>(&self, provider: &P, hash: &Hash) -> Result<bool, BlockchainError> {
        self.is_side_block_internal(provider, hash, self.get_topo_height()).await
    }

    // a block is a side block if its ordered and its block height is less than or equal to height of past 8 topographical blocks
    pub async fn is_side_block_internal<P>(&self, provider: &P, hash: &Hash, current_topoheight: TopoHeight) -> Result<bool, BlockchainError>
    where
        P: DifficultyProvider + DagOrderProvider
    {
        trace!("is block {} a side block", hash);
        if !provider.is_block_topological_ordered(hash).await {
            return Ok(false)
        }

        let topoheight = provider.get_topo_height_for_hash(hash).await?;
        // genesis block can't be a side block
        if topoheight == 0 || topoheight > current_topoheight {
            return Ok(false)
        }

        let height = provider.get_height_for_block_hash(hash).await?;

        // verify if there is a block with height higher than this block in past 8 topo blocks
        let mut counter = 0;
        let mut i = topoheight - 1;
        while counter < STABLE_LIMIT && i > 0 {
            let hash = provider.get_hash_at_topo_height(i).await?;
            let previous_height = provider.get_height_for_block_hash(&hash).await?;

            if height <= previous_height {
                return Ok(true)
            }
            counter += 1;
            i -= 1;
        }

        Ok(false)
    }

    // to have stable order: it must be ordered, and be under the stable height limit
    pub async fn has_block_stable_order<P>(&self, provider: &P, hash: &Hash, topoheight: TopoHeight) -> Result<bool, BlockchainError>
    where
        P: DagOrderProvider
    {
        trace!("has block {} stable order at topoheight {}", hash, topoheight);
        if provider.is_block_topological_ordered(hash).await {
            let block_topo_height = provider.get_topo_height_for_hash(hash).await?;
            return Ok(block_topo_height + STABLE_LIMIT <= topoheight)
        }
        Ok(false)
    }

    // Rewind the chain by removing N blocks from the top
    pub async fn rewind_chain(&self, count: u64, until_stable_height: bool) -> Result<(TopoHeight, Vec<(Hash, Arc<Transaction>)>), BlockchainError> {
        trace!("rewind chain of {} blocks (stable height: {})", count, until_stable_height);
        let mut storage = self.storage.write().await;
        self.rewind_chain_for_storage(&mut storage, count, until_stable_height).await
    }

    // Rewind the chain by removing N blocks from the top
    pub async fn rewind_chain_for_storage(&self, storage: &mut S, count: u64, stop_at_stable_height: bool) -> Result<(TopoHeight, Vec<(Hash, Arc<Transaction>)>), BlockchainError> {
        trace!("rewind chain with count = {}", count);
        let current_height = self.get_height();
        let current_topoheight = self.get_topo_height();
        warn!("Rewind chain with count = {}, height = {}, topoheight = {}", count, current_height, current_topoheight);
        let mut until_topo_height = if stop_at_stable_height {
            self.get_stable_topoheight()
        } else {
            0
        };

        for hash in self.checkpoints.iter() {
            if storage.is_block_topological_ordered(hash).await {
                let topo = storage.get_topo_height_for_hash(hash).await?;
                if until_topo_height <= topo {
                    info!("Configured checkpoint {} is at topoheight {}. Prevent to rewind below", hash, topo);
                    until_topo_height = topo;
                }
            }
        }

        let (new_height, new_topoheight, mut txs) = storage.pop_blocks(current_height, current_topoheight, count, until_topo_height).await?;
        debug!("New topoheight: {} (diff: {})", new_topoheight, current_topoheight - new_topoheight);

        // Clean mempool from old txs if the DAG has been updated
        {
            let mut mempool = self.mempool.write().await;
            txs.extend(mempool.drain());
        }

        // Try to add all txs back to mempool if possible
        // We try to prevent lost/to be orphaned
        // We try to add back all txs already in mempool just in case
        let mut orphaned_txs = Vec::new();
        {
            for (hash, tx) in txs {
                debug!("Trying to add TX {} to mempool again", hash);
                if let Err(e) = self.add_tx_to_mempool_with_storage_and_hash(storage, tx.clone(), Immutable::Owned(hash.clone()), false).await {
                    debug!("TX {} rewinded is not compatible anymore: {}", hash, e);
                    orphaned_txs.push((hash, tx));
                }
            }
        }

        self.height.store(new_height, Ordering::Release);
        self.topoheight.store(new_topoheight, Ordering::Release);
        // update stable height if it's allowed
        if !stop_at_stable_height {
            let tips = storage.get_tips().await?;
            let (stable_hash, stable_height) = self.find_common_base::<S, _>(&storage, &tips).await?;
            let stable_topoheight = storage.get_topo_height_for_hash(&stable_hash).await?;

            // if we have a RPC server, propagate the StableHeightChanged if necessary
            if let Some(rpc) = self.rpc.read().await.as_ref() {
                let previous_stable_height = self.get_stable_height();
                let previous_stable_topoheight = self.get_stable_topoheight();

                if stable_height != previous_stable_height {
                    if rpc.is_event_tracked(&NotifyEvent::StableHeightChanged).await {
                        let rpc = rpc.clone();
                        spawn_task("rpc-notify-stable-height", async move {
                            let event = json!(StableHeightChangedEvent {
                                previous_stable_height,
                                new_stable_height: stable_height
                            });
    
                            if let Err(e) = rpc.notify_clients(&NotifyEvent::StableHeightChanged, event).await {
                                debug!("Error while broadcasting event StableHeightChanged to websocket: {}", e);
                            }
                        });
                    }
                }

                if stable_topoheight != previous_stable_topoheight {
                    if rpc.is_event_tracked(&NotifyEvent::StableTopoHeightChanged).await {
                        let rpc = rpc.clone();
                        spawn_task("rpc-notify-stable-topoheight", async move {
                            let event = json!(StableTopoHeightChangedEvent {
                                previous_stable_topoheight,
                                new_stable_topoheight: stable_topoheight
                            });
    
                            if let Err(e) = rpc.notify_clients(&NotifyEvent::StableTopoHeightChanged, event).await {
                                debug!("Error while broadcasting event StableTopoHeightChanged to websocket: {}", e);
                            }
                        });
                    }
                }
            }
            self.stable_height.store(stable_height, Ordering::SeqCst);
            self.stable_topoheight.store(stable_topoheight, Ordering::SeqCst);
        }

        self.clear_caches().await;

        Ok((new_topoheight, orphaned_txs))
    }

    // Calculate the average block time on the last 50 blocks
    // It will return the target block time if we don't have enough blocks
    // We calculate it by taking the timestamp of the block at topoheight - 50 and the timestamp of the block at topoheight
    // It is the same as computing the average time between the last 50 blocks but much faster
    // Genesis block timestamp isn't take in count for this calculation
    pub async fn get_average_block_time<P>(&self, provider: &P) -> Result<TimestampMillis, BlockchainError>
    where
        P: DifficultyProvider + PrunedTopoheightProvider + DagOrderProvider
    {
        // current topoheight
        let topoheight = self.get_topo_height();

        // we need to get the block hash at topoheight - 50 to compare
        // if topoheight is 0, returns the target as we don't have any block
        // otherwise returns topoheight
        let mut count = if topoheight > 50 {
            50
        } else if topoheight <= 1 {
            return Ok(BLOCK_TIME_MILLIS);
        } else {
            topoheight - 1
        };

        // check that we are not under the pruned topoheight
        if let Some(pruned_topoheight) = provider.get_pruned_topoheight().await? {
            if topoheight - count < pruned_topoheight {
                count = pruned_topoheight
            }
        }

        let now_hash = provider.get_hash_at_topo_height(topoheight).await?;
        let now_timestamp = provider.get_timestamp_for_block_hash(&now_hash).await?;

        let count_hash = provider.get_hash_at_topo_height(topoheight - count).await?;
        let count_timestamp = provider.get_timestamp_for_block_hash(&count_hash).await?;

        let diff = now_timestamp - count_timestamp;
        Ok(diff / count)
    }
}

// Estimate the required fees for a transaction using energy-based model
// For transfer transactions, energy can be used for free transfers
// For non-transfer transactions, TOS fees are required
pub async fn estimate_required_tx_fees<P: AccountProvider>(provider: &P, current_topoheight: TopoHeight, tx: &Transaction, _: BlockVersion) -> Result<u64, BlockchainError> {
    let mut output_count = 0;
    let mut processed_keys = HashSet::new();
    if let TransactionType::Transfers(transfers) = tx.get_data() {
        output_count = transfers.len();
        for transfer in transfers {
            if !provider.is_account_registered_for_topoheight(transfer.get_destination(), current_topoheight).await? {
                debug!("Account {} is not registered for topoheight {}", transfer.get_destination().as_address(provider.is_mainnet()), current_topoheight);
                processed_keys.insert(transfer.get_destination());
            }
        }
    }

    // For transfer transactions, use energy-based fee calculation
    // For non-transfer transactions, use TOS-based fee calculation
    let fee = if matches!(tx.get_data(), TransactionType::Transfers(_)) {
        // Energy-based fee calculation for transfers
        calculate_energy_fee(
            tx.size(), 
            output_count, 
            processed_keys.len()
        )
    } else {
        // TOS-based fee calculation for non-transfer transactions
        // Use traditional fee calculation for non-transfer operations
        calculate_tx_fee(
            tx.size(), 
            output_count, 
            processed_keys.len(),
            tx.get_multisig_count()
        )
    };

    Ok(fee)
}

// Get the block reward for a side block based on how many side blocks exists at same height
pub fn side_block_reward_percentage(side_blocks: u64) -> u64 {
    let mut side_block_percent = SIDE_BLOCK_REWARD_PERCENT;
    if side_blocks > 0 {
        if side_blocks < SIDE_BLOCK_REWARD_MAX_BLOCKS {
            side_block_percent = SIDE_BLOCK_REWARD_PERCENT / (side_blocks * 2);
        } else {
            // If we have more than 3 side blocks at same height
            // we reduce the reward to 5%
            side_block_percent = SIDE_BLOCK_REWARD_MIN_PERCENT;
        }
    }

    side_block_percent
}

// Calculate the block reward based on the emitted supply
pub fn get_block_reward(supply: u64) -> u64 {
    // Prevent any overflow
    if supply >= MAXIMUM_SUPPLY {
        // Max supply reached, do we want to generate small fixed amount of coins? 
        return 0
    }

    let base_reward = (MAXIMUM_SUPPLY - supply) >> EMISSION_SPEED_FACTOR;
    base_reward * BLOCK_TIME_MILLIS / MILLIS_PER_SECOND / 180
}

// Returns the fee percentage for a block at a given height
pub fn get_block_dev_fee(height: u64) -> u64 {
    let mut percentage = 0;
    for threshold in DEV_FEES.iter() {
        if height >= threshold.height {
            percentage = threshold.fee_percentage;
        }
    }

    percentage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reward_side_block_percentage() {
        assert_eq!(side_block_reward_percentage(0), SIDE_BLOCK_REWARD_PERCENT);
        assert_eq!(side_block_reward_percentage(1), SIDE_BLOCK_REWARD_PERCENT / 2);
        assert_eq!(side_block_reward_percentage(2), SIDE_BLOCK_REWARD_PERCENT / 4);
        assert_eq!(side_block_reward_percentage(3), SIDE_BLOCK_REWARD_MIN_PERCENT);
    }

    #[test]
    fn test_block_dev_fee() {
        assert_eq!(get_block_dev_fee(0), 10);
        assert_eq!(get_block_dev_fee(1), 10);

        // ~ current height
        assert_eq!(get_block_dev_fee(55_000), 10);

        // End of the first threshold, we pass to 5%
        assert_eq!(get_block_dev_fee(3_942_000), 5);

        assert_eq!(get_block_dev_fee(DEV_FEES[0].height), 10);
        assert_eq!(get_block_dev_fee(DEV_FEES[1].height), 5);
        assert_eq!(get_block_dev_fee(DEV_FEES[1].height + 1), 5);
    }
}