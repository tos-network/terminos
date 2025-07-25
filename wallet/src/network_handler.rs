use std::{
    borrow::Cow,
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant}
};
use futures::{stream, StreamExt, TryStreamExt};
use indexmap::IndexMap;
use thiserror::Error;
use anyhow::{Context, Error};
use log::{debug, error, info, trace, warn};
use terminos_common::{
    account::CiphertextCache,
    api::{
        daemon::{
            BlockResponse,
            MultisigState,
            NewBlockEvent,
            RPCBlockResponse
        },
        wallet::BalanceChanged,
        RPCTransactionType
    },
    config::TERMINOS_ASSET,
    crypto::{
        elgamal::Ciphertext,
        Address,
        Hash
    },
    tokio::{
        select,
        spawn_task,
        sync::{mpsc::channel, Mutex},
        task::{JoinError, JoinHandle},
        time::sleep
    },
    transaction::{
        extra_data::{PlaintextExtraData, PlaintextFlag},
        ContractDeposit,
        MultiSigPayload,
        Role
    },
    utils::sanitize_ws_address
};
use crate::{
    config::AUTO_RECONNECT_INTERVAL,
    daemon_api::DaemonAPI,
    entry::{
        DeployInvoke,
        EntryData,
        TransactionEntry,
        TransferIn,
        TransferOut
    },
    storage::{Balance, MultiSig},
    wallet::{Event, Wallet}
};

// NetworkHandler must be behind a Arc to be accessed from Wallet (to stop it) or from tokio task
pub type SharedNetworkHandler = Arc<NetworkHandler>;

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("network handler is already running")]
    AlreadyRunning,
    #[error("network handler is not running")]
    NotRunning,
    #[error(transparent)]
    TaskError(#[from] JoinError),
    #[error(transparent)]
    DaemonAPIError(#[from] Error),
    #[error("Network mismatch")]
    NetworkMismatch
}

pub struct NetworkHandler {
    // tokio task
    task: Mutex<Option<JoinHandle<Result<(), Error>>>>,
    // wallet where we can save every data from chain
    wallet: Arc<Wallet>,
    // api to communicate with daemon
    // It is behind a Arc to be shared across several wallets
    // in case someone make a custom service and don't want to create a new connection
    api: Arc<DaemonAPI>,
    // Concurrency to use during syncing
    concurrency: usize,
}

impl NetworkHandler {
    // Create a new network handler with a wallet and a daemon address
    // This will create itself a DaemonAPI and verify if connection is possible
    pub async fn new<S: ToString>(wallet: Arc<Wallet>, daemon_address: S, concurrency: usize) -> Result<SharedNetworkHandler, Error> {
        let s = daemon_address.to_string();
        let api = DaemonAPI::new(format!("{}/json_rpc", sanitize_ws_address(s.as_str()))).await?;
        Self::with_api(wallet, Arc::new(api), concurrency).await
    }

    // Create a new network handler with an already created daemon API
    pub async fn with_api(wallet: Arc<Wallet>, api: Arc<DaemonAPI>, concurrency: usize) -> Result<SharedNetworkHandler, Error> {
        // check that we can correctly get version from daemon
        let version = api.get_version().await?;
        debug!("Connected to daemon running version {}", version);

        Ok(Arc::new(Self {
            task: Mutex::new(None),
            wallet,
            api,
            concurrency
        }))
    }

    // Start the internal loop to sync all missed blocks and all newly added blocks
    pub async fn start(self: &Arc<Self>, auto_reconnect: bool) -> Result<(), NetworkError> {
        trace!("Starting network handler");

        if self.is_running().await {
            return Err(NetworkError::AlreadyRunning)
        }

        if !self.api.is_online() {
            debug!("API is offline, trying to reconnect #1");
            if !self.api.reconnect().await? {
                error!("Couldn't reconnect to server");
                return Err(NetworkError::NotRunning)
            }
        }

        let zelf = Arc::clone(&self);
        *self.task.lock().await = Some(spawn_task("network-handler", async move {
            loop {
                // Notify that we are online
                zelf.wallet.propagate_event(Event::Online).await;

                let res =  zelf.start_syncing().await;
                if let Err(e) = res.as_ref() {
                    error!("Error while syncing: {}", e);
                    zelf.wallet.propagate_event(Event::SyncError { message: e.to_string() }).await;
                }

                // Notify that we are offline
                zelf.wallet.propagate_event(Event::Offline).await;

                if !auto_reconnect {
                    // Turn off the websocket connection
                    if let Err(e) = zelf.api.disconnect().await {
                        debug!("Error while closing websocket connection: {}", e);
                    }

                    break res;
                } else {
                    if !zelf.api.is_online() {
                        debug!("API is offline, trying to reconnect #2");
                        if !zelf.api.reconnect().await? {
                            error!("Couldn't reconnect to server, trying again in {} seconds", AUTO_RECONNECT_INTERVAL);
                            sleep(Duration::from_secs(AUTO_RECONNECT_INTERVAL)).await;
                        }
                    } else {
                        warn!("Daemon is online but we couldn't sync, trying again in {} seconds", AUTO_RECONNECT_INTERVAL);
                        sleep(Duration::from_secs(AUTO_RECONNECT_INTERVAL)).await;
                    }
                }
            }
        }));

        Ok(())
    }

    // Stop the internal loop to stop syncing
    pub async fn stop(&self, api: bool) -> Result<(), NetworkError> {
        trace!("Stopping network handler");
        if let Some(handle) = self.task.lock().await.take() {
            if handle.is_finished() {
                debug!("Network handler is already finished");
                // We are already finished, which mean the event got triggered
                handle.await??;
            } else {
                debug!("Network handler is running, stopping it");
                handle.abort();

                // Notify that we are offline
                self.wallet.propagate_event(Event::Offline).await;
            }

            if api {
                debug!("Network handler stopped, disconnecting api");
                // Turn off the websocket connection
                if let Err(e) = self.api.disconnect().await {
                    debug!("Error while closing websocket connection: {}", e);
                }
            }

            Ok(())
        } else {
            Err(NetworkError::NotRunning)
        }
    }

    // Retrieve the daemon API used
    pub fn get_api(&self) -> &DaemonAPI {
        &self.api
    }

    // check if the network handler is running (that we have a task and its not finished)
    pub async fn is_running(&self) -> bool {
        let task = self.task.lock().await;
        if let Some(handle) = task.as_ref() {
            !handle.is_finished() && self.api.is_online()
        } else {
            false
        }
    }

    // Process a block by checking if it contains any transaction for us
    // Or that we mined it
    // Returns assets that changed and returns the highest nonce if we send a transaction
    async fn process_block(&self, address: &Address, block: BlockResponse, topoheight: u64) -> Result<Option<(HashSet<Hash>, Option<u64>)>, Error> {
        let block_hash = block.hash.into_owned();
        debug!("Processing block {} at topoheight {}", block_hash, topoheight);

        if block.miner.is_mainnet() != self.wallet.get_network().is_mainnet() {
            debug!("Block {} at topoheight {} is not on the same network as the wallet", block_hash, topoheight);
            return Err(NetworkError::NetworkMismatch.into())
        }

        let mut assets_changed = HashSet::new();
        // Miner address to verify if we mined the block
        let miner = block.miner.into_owned().to_public_key();
        let should_scan_history = self.wallet.get_history_scan();

        // Prevent storing changes multiple times
        let mut changes_stored = false;

        // create Coinbase entry if its our address and we're looking for TOS asset
        if miner == *address.get_public_key() {
            debug!("Block {} at topoheight {} is mined by us", block_hash, topoheight);
            if let Some(reward) = block.miner_reward {
                assets_changed.insert(TERMINOS_ASSET);
                
                if should_scan_history {
                    let coinbase = EntryData::Coinbase { reward };
                    let entry = TransactionEntry::new(block_hash.clone(), topoheight, block.timestamp, coinbase);

                    let broadcast = {
                        let mut storage = self.wallet.get_storage().write().await;
    
                        // Mark it as last coinbase reward topoheight
                        // it is internally checked if its higher or not
                        debug!("Storing last coinbase reward topoheight {}", topoheight);
                        storage.set_last_coinbase_reward_topoheight(Some(topoheight))?;
    
                        if storage.has_transaction(entry.get_hash())? {
                            false
                        } else {
                            storage.save_transaction(entry.get_hash(), &entry)?;
        
                            // Store the changes for history
                            if !changes_stored {
                                storage.add_topoheight_to_changes(topoheight, &block_hash)?;
                                changes_stored = true;
                            }
                            true
                        }
                    };
    
                    // Propagate the event to the wallet
                    if broadcast {
                        self.wallet.propagate_event(Event::NewTransaction(entry.serializable(self.wallet.get_network().is_mainnet()))).await;
                    }
                }
            } else {
                warn!("No reward for block {} at topoheight {}", block_hash, topoheight);
            }
        }

        // Highest nonce we found in this block
        let mut our_highest_nonce = None;

        let block_hash = &block_hash;
        let results: Vec<(Option<(TransactionEntry, u64, Option<u64>)>, HashSet<Hash>)> = stream::iter(block.transactions.into_iter())
            .map(|tx| async move {
                let mut assets_changed = HashSet::new();

                trace!("Checking transaction {}", tx.hash);
                let is_owner = *tx.source.get_public_key() == *address.get_public_key();
                if is_owner {
                    debug!("Transaction {} is from us", tx.hash);
                    assets_changed.insert(TERMINOS_ASSET);
                }

                let entry: Option<EntryData> = match tx.data {
                    RPCTransactionType::Burn(payload) => {
                        let payload = payload.into_owned();
                        if is_owner {
                            assets_changed.insert(payload.asset.clone());

                            if !should_scan_history || self.has_tx_stored(&tx.hash).await? {
                                debug!("Transaction burn {} was already stored, skipping it", tx.hash);
                                return Ok((None, assets_changed));
                            }
    
                            Some(EntryData::Burn { asset: payload.asset, amount: payload.amount, fee: tx.fee, nonce: tx.nonce })
                        } else {
                            None
                        }
                    },
                    RPCTransactionType::Transfers(txs) => {
                        let mut transfers_in: Vec<TransferIn> = Vec::new();
                        let mut transfers_out: Vec<TransferOut> = Vec::new();
    
                        // Used to check only once if we have processed this TX already
                        let mut checked = false;
                        for (i, transfer) in txs.into_iter().enumerate() {
                            let destination = transfer.destination.to_public_key();
                            if is_owner || destination == *address.get_public_key() {
                                let asset = transfer.asset.into_owned();
                                assets_changed.insert(asset.clone());

                                // Check only once if we have processed this TX already
                                if !checked {
                                    // Check if we already stored this TX
                                    if self.has_tx_stored(&tx.hash).await? {
                                        debug!("Transaction {} was already stored, skipping it", tx.hash);
                                        return Ok((None, assets_changed));
                                    }
                                    checked = true;
                                }

                                if !should_scan_history {
                                    continue;
                                }

                                // Get the right handle
                                let (role, handle) = if is_owner {
                                    (Role::Sender, transfer.sender_handle)
                                } else {
                                    (Role::Receiver, transfer.receiver_handle)
                                };
    
                                // Decompress commitment it if possible
                                let commitment = match transfer.commitment.decompress() {
                                    Ok(c) => c,
                                    Err(e) => {
                                        error!("Error while decompressing commitment of TX {}: {}", tx.hash, e);
                                        continue;
                                    }
                                };
    
                                // Same for handle
                                let handle = match handle.decompress() {
                                    Ok(h) => h,
                                    Err(e) => {
                                        error!("Error while decompressing handle of TX {}: {}", tx.hash, e);
                                        continue;
                                    }
                                };
    
                                let extra_data = if let Some(cipher) = transfer.extra_data.into_owned() {
                                    match self.wallet.decrypt_extra_data(cipher,  Some(&handle), role, tx.version) {
                                        Ok(e) => Some(e),
                                        Err(e) => {
                                            warn!("Error while decrypting extra data of TX {}: {}", tx.hash, e);
                                            Some(PlaintextExtraData::new(None, None, PlaintextFlag::Failed))
                                        }
                                    }
                                } else {
                                    None
                                };

                                debug!("Decrypting amount from TX {} of asset {}", tx.hash, asset);
                                let ciphertext = Ciphertext::new(commitment, handle);
                                let amount = match self.wallet.decrypt_ciphertext_of_asset(ciphertext, &asset).await? {
                                    Some(v) => v,
                                    None => {
                                        warn!("Couldn't decrypt the ciphertext of transfer #{} for asset {} in TX {}. Skipping it", i, asset, tx.hash);
                                        continue;
                                    }
                                };
    
                                if is_owner {
                                    let transfer = TransferOut::new(destination, asset, amount, extra_data);
                                    transfers_out.push(transfer);
                                } else {
                                    let transfer = TransferIn::new(asset, amount, extra_data);
                                    transfers_in.push(transfer);
                                }
                            }
                        }
    
                        if is_owner && !transfers_out.is_empty() { // check that we are owner of this TX
                            Some(EntryData::Outgoing { transfers: transfers_out, fee: tx.fee, nonce: tx.nonce })
                        } else if !transfers_in.is_empty() { // otherwise, check that we received one or few transfers from it
                            Some(EntryData::Incoming { from: tx.source.to_public_key(), transfers: transfers_in })
                        } else { // this TX has nothing to do with us, nothing to save
                            None
                        }
                    },
                    RPCTransactionType::MultiSig(payload) => {
                        let payload = payload.into_owned();
                        if is_owner {
                            if !should_scan_history || self.has_tx_stored(&tx.hash).await? {
                                debug!("Transaction multisig setup {} was already stored, skipping it", tx.hash);
                                return Ok((None, assets_changed));
                            }
    
                            Some(EntryData::MultiSig { participants: payload.participants, threshold: payload.threshold, fee: tx.fee, nonce: tx.nonce })
                        } else {
                            None
                        }
                    },
                    RPCTransactionType::InvokeContract(payload) => {
                        let payload = payload.into_owned();
                        if is_owner {
                            if self.has_tx_stored(&tx.hash).await? {
                                debug!("Transaction invoke contract {} was already stored, skipping it", tx.hash);
                                return Ok((None, assets_changed));
                            }
    
                            let mut deposits = IndexMap::new();
                            for (asset, deposit) in payload.deposits {
                                assets_changed.insert(asset.clone());

                                if should_scan_history {
                                    match deposit {
                                        ContractDeposit::Public(amount) => {
                                            deposits.insert(asset, amount);
                                        },
                                        ContractDeposit::Private { commitment, sender_handle, ..} => {
                                            let commitment = commitment.decompress()?;
                                            let handle = sender_handle.decompress()?;
                                            let ciphertext = Ciphertext::new(commitment, handle);
                                            let amount = match self.wallet.decrypt_ciphertext_of_asset(ciphertext, &asset).await? {
                                                Some(v) => v,
                                                None => {
                                                    warn!("Couldn't decrypt deposit ciphertext for asset {}. Skipping it", asset);
                                                    continue;
                                                }
                                            };
                                            deposits.insert(asset, amount);
                                        }
                                    }
                                }
                            }

                            if should_scan_history {
                                Some(EntryData::InvokeContract { contract: payload.contract, deposits, chunk_id: payload.chunk_id, fee: tx.fee, max_gas: payload.max_gas, nonce: tx.nonce })
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    },
                    RPCTransactionType::DeployContract(payload) => {
                        if is_owner {
                            if self.has_tx_stored(&tx.hash).await? {
                                debug!("Transaction deploy contract {} was already stored, skipping it", tx.hash);
                                return Ok((None, assets_changed));
                            }

                            let payload = payload.into_owned();
                            let invoke = if let Some(invoke) = payload.invoke {
                                let mut deposits = IndexMap::new();
                                for (asset, deposit) in invoke.deposits {
                                    assets_changed.insert(asset.clone());

                                    if should_scan_history {
                                        match deposit {
                                            ContractDeposit::Public(amount) => {
                                                deposits.insert(asset, amount);
                                            },
                                            ContractDeposit::Private { commitment, sender_handle, ..} => {
                                                let commitment = commitment.decompress()?;
                                                let handle = sender_handle.decompress()?;
                                                let ciphertext = Ciphertext::new(commitment, handle);
                                                let amount = match self.wallet.decrypt_ciphertext_of_asset(ciphertext, &asset).await? {
                                                    Some(v) => v,
                                                    None => {
                                                        warn!("Couldn't decrypt deposit ciphertext for asset {}. Skipping it", asset);
                                                        continue;
                                                    }
                                                };
                                                deposits.insert(asset, amount);
                                            }
                                        }
                                    }
                                }

                                Some(DeployInvoke {
                                    max_gas: invoke.max_gas,
                                    deposits,
                                })
                            } else {
                                None
                            };

                            if should_scan_history {
                                Some(EntryData::DeployContract { fee: tx.fee, nonce: tx.nonce, invoke })
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    },
                    RPCTransactionType::Energy(_) => {
                        // Energy transactions are not yet supported in wallet history
                        None
                    }
                };

                let result = if let Some(entry) = entry {
                    // Transaction found at which topoheight it was executed
                    let mut tx_topoheight = topoheight;
                    let mut tx_timestamp = block.timestamp;
    
                    // New transaction entry that may be linked to us, check if TX was executed
                    if !self.api.is_tx_executed_in_block(&tx.hash, &block_hash).await? {
                        debug!("Transaction {} was a good candidate but was not executed in block {}, searching its block executor", tx.hash, block_hash);
                        // Don't skip the TX, we may have missed it
                        match self.api.get_transaction_executor(&tx.hash).await {
                            Ok(executor) => {
                                tx_topoheight = executor.block_topoheight;
                                tx_timestamp = executor.block_timestamp;
                                debug!("Transaction {} was executed in block {} at topoheight {}", tx.hash, executor.block_hash, executor.block_topoheight);
                            },
                            Err(e) => {
                                // Tx is maybe not executed, this is really rare event
                                warn!("Error while fetching topoheight execution of transaction {}: {}", tx.hash, e);
                                return Ok((None, assets_changed));
                            }
                        }
                    }

                    // Save the transaction
                    let entry = TransactionEntry::new(tx.hash.into_owned(), tx_topoheight, tx_timestamp, entry);
                    let tx_nonce = if is_owner{
                        Some(tx.nonce)
                    } else {
                        None
                    };

                    Some((entry, tx_topoheight, tx_nonce))
                } else {
                    None
                };

                Ok::<_, Error>((result, assets_changed))
            })
            .boxed()
            .buffered(self.concurrency)
            .try_collect()
            .await?;

        for (entry, changes) in results {
                assets_changed.extend(changes);

                if let Some((entry, tx_topoheight, tx_nonce)) = entry {
                    // Find the highest nonce
                    if let Some(tx_nonce) = tx_nonce {
                        if our_highest_nonce.map(|n| tx_nonce > n).unwrap_or(true) {
                            debug!("Found new highest nonce {} in TX {}", tx_nonce, entry.get_hash());
                            our_highest_nonce = Some(tx_nonce);
                        }
                    }

                    debug!("storing new entry {} from block {}", entry.get_hash(), block_hash);
                    {
                        let mut storage = self.wallet.get_storage().write().await;
                        storage.save_transaction(entry.get_hash(), &entry)?;
                        // Store the changes for history
                        if !changes_stored {
                            debug!("mark topoheight {} as changed", topoheight);
                            storage.add_topoheight_to_changes(topoheight, &block_hash)?;
                            changes_stored = true;
                        }

                        if let EntryData::MultiSig { participants, threshold, .. } = entry.get_entry() {
                            let multisig = MultiSig {
                                payload: MultiSigPayload {
                                    participants: participants.clone(),
                                    threshold: *threshold
                                },
                                topoheight: tx_topoheight
                            };
                            let store = storage.get_multisig_state().await?
                                .map(|m| m.topoheight < tx_topoheight)
                                .unwrap_or(true);
        
                            if store {
                                info!("Detected a multisig state change at topoheight {} from TX {}", tx_topoheight, entry.get_hash());
                                if multisig.payload.is_delete() {
                                    info!("Deleting multisig state");
                                    storage.delete_multisig_state().await?;
                                } else {
                                    info!("Updating multisig state");
                                    storage.set_multisig_state(multisig).await?;
                                }
                            }
                        }
                    }

                    // Propagate the event to the wallet
                    self.wallet.propagate_event(Event::NewTransaction(entry.serializable(self.wallet.get_network().is_mainnet()))).await;
                }
            }

        // Also, verify the block version, so we handle smoothly a change in TX Version
        {
            let tx_version = block.version.get_tx_version();
            let mut storage = self.wallet.get_storage().write().await;
            if storage.get_tx_version().await? < tx_version {
                info!("Updating TX version to {}", tx_version);
                storage.set_tx_version(tx_version).await?;
            }
        }

        if !changes_stored || assets_changed.is_empty() {
            debug!("No changes found in block {} at topoheight {}, assets: {}, changes stored: {}", block_hash, topoheight, assets_changed.len(), changes_stored);
            Ok(None)
        } else {
            // Increase by one to get the new nonce
            Ok(Some((assets_changed, our_highest_nonce.map(|n| n + 1))))
        }
    }

    // Check if a transaction is stored in the wallet
    #[inline]
    async fn has_tx_stored(&self, hash: &Hash) -> Result<bool, Error> {
        let storage = self.wallet.get_storage().read().await;
        storage.has_transaction(hash)
    }

    // Scan the chain using a specific balance asset, this helps us to get a list of version to only requests blocks where changes happened
    // When the block is requested, we don't limit the syncing to asset in parameter
    async fn get_balance_and_transactions(&self, topoheight_processed: Arc<Mutex<HashSet<u64>>>, address: &Address, asset: &Hash, min_topoheight: u64, balances: bool, highest_nonce: &mut Option<u64>) -> Result<(), Error> {
        // Retrieve the highest version
        let (topoheight, version) = self.api.get_balance(address, asset).await
            .map(|res| (res.topoheight, res.version))?;

        debug!("Starting sync from topoheight {} for asset {}", topoheight, asset);

        // don't sync already synced blocks
        if min_topoheight >= topoheight {
            debug!("Reached minimum topoheight {}, topo: {}", min_topoheight, topoheight);
            return Ok(())
        }

        // Determine if its the highest version of balance or not
        // This is used to save the latest balance
        let mut highest_version = true;

        // This channel is used to send all the blocks to the processing loop
        // No more than {concurrency} blocks and versions will be prefetch in advance
        // as the task will automatically await on the channel
        let (data_sender, mut data_receiver) = channel::<(CiphertextCache, u64, Option<RPCBlockResponse<'static>>)>(self.concurrency);
        let handle = {
            let api = self.api.clone();
            let address = address.clone();
            let asset = asset.clone();
            spawn_task("fetch-asset-versions", async move {
                let mut version = Some(version);
                let mut topoheight = topoheight;
                while let Some(v) = version.take() {
                    let (balance, _, _, previous_topoheight) = v.consume();
                    let block = if {
                        let mut lock = topoheight_processed.lock().await;
                        lock.insert(topoheight)
                    } {
                        trace!("fetching block with txs at {}", topoheight);
                        Some(api.get_block_with_txs_at_topoheight(topoheight).await?)
                    } else {
                        None
                    };
                    data_sender.send((balance, topoheight, block)).await?;

                    if let Some(previous) = previous_topoheight {
                        // don't sync already synced blocks
                        if previous > min_topoheight {
                            trace!("fetch next version at {}", previous);
                            topoheight = previous;
                            version = Some(api.get_balance_at_topoheight(&address, &asset, previous).await?);
                        }
                    }
                }

                Ok::<_, Error>(())
            })
        };

        while let Some((mut balance, topoheight, block)) = data_receiver.recv().await {
            // add this topoheight in cache to not re-process it (blocks are independant of asset to have faster sync)
            // if its not already processed, do it
            if let Some(response) = block {
                debug!("Processing topoheight {}", topoheight);
                let changes = self.process_block(address, response, topoheight).await?;

                // Check if a change occured, we are the highest version and update balances is requested
                if let Some((_, nonce)) = changes.filter(|_| balances && highest_version) {
                    let mut storage = self.wallet.get_storage().write().await;

                    {
                        // Set the highest nonce we know
                        if highest_nonce.is_none() {
                            // Get the highest nonce from storage
                            debug!("Highest nonce is not set, fetching it from storage");
                            *highest_nonce = Some(storage.get_nonce()?);
                        }
    
                        // Store only the highest nonce
                        // Because if we are building queued transactions, it may break our queue
                        // Our we couldn't submit new txs before they get removed from mempool
                        if let Some(nonce) = nonce.filter(|n| highest_nonce.as_ref().map(|h| *h < *n).unwrap_or(true)) {
                            debug!("Storing new highest nonce {}", nonce);
                            storage.set_nonce(nonce)?;
                            *highest_nonce = Some(nonce);
                        }
                    }

                    // If we have no balance in storage OR the stored ciphertext isn't the same, we should store it
                    let store = storage.get_balance_for(asset).await.map(|b| b.ciphertext != balance).unwrap_or(true);
                    if store {
                        let plaintext_balance = if let Some(plaintext_balance) = storage.get_unconfirmed_balance_decoded_for(&asset, &balance.compressed()).await? {
                            plaintext_balance
                        } else {
                            trace!("Decrypting balance for asset {}", asset);
                            let ciphertext = balance.decompressed()?;
                            let max_supply = storage.get_asset(asset).await?
                                .get_max_supply();

                            self.wallet.decrypt_ciphertext_with(ciphertext.clone(), max_supply).await?
                                .context(format!("Couldn't decrypt the ciphertext for {} at topoheight {}", asset, topoheight))?
                        };

                        debug!("Storing balance from topoheight {} for asset {} ({}) {}", topoheight, asset, balance, plaintext_balance);
                        // Store the new balance
                        storage.set_balance_for(asset, Balance::new(plaintext_balance, balance)).await?;

                        // Propagate the event
                        self.wallet.propagate_event(Event::BalanceChanged(BalanceChanged {
                            asset: asset.clone(),
                            balance: plaintext_balance
                        })).await;
                    }
                }
            }

            // Only first iteration is the highest one
            highest_version = false;
        }

        handle.await??;

        Ok(())
    }

    // Locate the last topoheight valid for syncing, this support soft forks, DAG reorgs, etc...
    // Balances and nonce may be outdated, but we will sync them later
    // All transactions / changes above the last valid topoheight will be deleted
    // Returns daemon topoheight along wallet stable topoheight and if back sync is needed
    async fn locate_sync_topoheight_and_clean(&self) -> Result<(u64, Hash, u64), NetworkError> {
        trace!("locating sync topoheight and cleaning");
        let info = self.api.get_info().await?;
        let daemon_topoheight = info.topoheight;
        let daemon_block_hash = info.top_block_hash;
        let pruned_topoheight = info.pruned_topoheight.unwrap_or(0);

        // Verify that we are on the same network
        {
            let network = self.wallet.get_network();
            if info.network != *network {
                error!("Network mismatch! Our network is {} while daemon is {}", network, info.network);
                return Err(NetworkError::NetworkMismatch)
            }
        }

        // Retrieve the highest point possible
        let synced_topoheight = {
            let storage = self.wallet.get_storage().read().await;
            if storage.has_top_block_hash()? {
                // Check that the daemon topoheight is the same as our
                // Verify also that the top block hash is same as our
                let top_block_hash = storage.get_top_block_hash()?;
                let synced_topoheight = storage.get_synced_topoheight()?;

                // Check if its the top
                if daemon_topoheight == synced_topoheight && daemon_block_hash == top_block_hash {
                    // No need to sync back, we are already synced
                    return Ok((daemon_topoheight, daemon_block_hash, synced_topoheight))
                }

                // Verify we are not above the daemon chain
                if synced_topoheight > daemon_topoheight {
                    warn!("We are above the daemon chain, we should sync from scratch");
                    return Ok((daemon_topoheight, daemon_block_hash, 0))
                }

                if synced_topoheight > pruned_topoheight {
                    // Check if it's still a correct block
                    let header = self.api.get_block_at_topoheight(synced_topoheight).await?;
                    let block_hash = header.hash.into_owned();
                    if block_hash == top_block_hash {
                        // topoheight and block hash are equal, we are still on right chain
                        return Ok((daemon_topoheight, daemon_block_hash, synced_topoheight))
                    }
                }

                synced_topoheight
            } else {
                storage.get_synced_topoheight().unwrap_or(0)
            }
        };

        // Search the highest block that is still valid for wallet
        let mut maximum = synced_topoheight;
        let block_hash = loop {
            maximum = {
                let storage = self.wallet.get_storage().read().await;
                storage.get_highest_topoheight_in_changes_below(maximum)?
            };

            // We are completely wrong, we should sync from scratch
            if maximum == 0 {
                break None;
            }

            // We are under the pruned topoheight,
            // lets assume we are on the right chain under it
            if maximum < pruned_topoheight {
                maximum = pruned_topoheight;
                break None;
            }

            // Retrieve local hash
            let local_hash = {
                let storage = self.wallet.get_storage().read().await;
                storage.get_block_hash_for_topoheight(maximum)?
            };

            // Check if we are on the same chain
            debug!("Checking if we are on the same chain at topoheight {}", maximum);
            let header = self.api.get_block_at_topoheight(maximum).await?;
            let block_hash = header.hash.into_owned();
            if block_hash == local_hash {
                break Some(local_hash);
            }

            // Looks like we are on a different chain
            maximum -= 1;
        };

        // Get the hash of the block at this topoheight
        let block_hash = if let Some(block_hash) = block_hash {
            block_hash
        } else {
            let response = self.api.get_block_at_topoheight(maximum).await?;
            response.hash.into_owned()
        };

        let mut storage = self.wallet.get_storage().write().await;
        // Now let's clean everything
        if storage.delete_changes_above_topoheight(maximum)? {
            warn!("Cleaning transactions above topoheight {}", maximum);
            // Changes were deleted, we should also delete transactions
            storage.delete_transactions_above_topoheight(maximum)?;
        }

        // Save the new values
        storage.set_synced_topoheight(maximum)?;
        storage.set_top_block_hash(&block_hash)?;
        // Add it only if its not already in changes
        if !storage.has_topoheight_in_changes(maximum)? {
            storage.add_topoheight_to_changes(maximum, &block_hash)?;
        }

        // Verify its not the first time we do a sync
        if synced_topoheight != 0 {
            self.wallet.propagate_event(Event::Rescan { start_topoheight: maximum }).await;   
        }

        Ok((daemon_topoheight, daemon_block_hash, maximum))
    }

    // Sync the latest version of our balances and nonces and determine if we should parse all blocks
    // If assets are provided, we'll only sync these assets
    // If nonce is not provided, we will fetch it from the daemon
    pub async fn sync_head_state(&self, address: &Address, assets: Option<HashSet<Hash>>, nonce: Option<u64>, sync_nonce: bool, sync_multisig: bool) -> Result<bool, Error> {
        trace!("syncing head state");
        let new_nonce = if sync_nonce {
            debug!("no nonce provided, fetching it from daemon");
            match self.api.get_nonce(&address).await.map(|v| v.version) {
                Ok(v) => Some(v.get_nonce()),
                Err(e) => {
                    debug!("Error while fetching last nonce: {}", e);
                    {
                        let mut storage = self.wallet.get_storage().write().await;
                        if storage.has_any_balance().await? {
                            warn!("We have balances but we couldn't fetch the nonce, deleting all balances");
                            storage.delete_balances().await?;
                            storage.delete_assets().await?;
                            storage.delete_multisig_state().await?;
                            storage.delete_nonce().await?;
                        }
                    }
                    // Account is not registered, we can return safely here
                    return Ok(false)
                }
            }
        } else {
            nonce
        };

        // Check if we have a multisig account
        if sync_multisig {
            if self.api.has_multisig(address).await.unwrap_or(false) {
                debug!("Multisig account detected");
                let data = self.api.get_multisig(address).await?;
                if let MultisigState::Active { participants, threshold } = data.state {
                    debug!("Active multisig account with participants [{}] and threshold {}", participants.iter().map(Address::to_string).collect::<Vec<_>>().join(", "), threshold);
    
                    let payload = MultiSigPayload {
                        participants: participants.into_iter().map(|p| p.to_public_key()).collect(),
                        threshold
                    };
    
                    let multisig = MultiSig {
                        payload,
                        topoheight: data.topoheight
                    };
                    let mut storage = self.wallet.get_storage().write().await;
                    storage.set_multisig_state(multisig).await?;
                } else {
                    warn!("Multisig account is not active while marked as, skipping it");
                }
            } else {
                let mut storage = self.wallet.get_storage().write().await;
                if storage.has_multisig_state().await? {
                    info!("No multisig account detected, deleting multisig state");
                    storage.delete_multisig_state().await?;
                }
            }
        }

        let detected_assets = if let Some(assets) = assets.as_ref().filter(|a| !a.is_empty()) {
            trace!("got {} assets requested", assets.len());
            let mut references = HashSet::new();
            references.extend(assets.iter().map(Cow::Borrowed));
            references
        } else {
            trace!("no assets provided, fetching all assets");
            let mut assets = HashSet::new();
            let mut skip = 0;

            loop {
                debug!("requesting account assets, skip {}", skip);
                let response = self.api.get_account_assets(address, None, Some(skip)).await?;
                if response.is_empty() {
                    break;
                }
                skip += response.len();
                assets.extend(response.into_iter().map(Cow::Owned));
            }

            if assets.is_empty() {
                let mut storage = self.wallet.get_storage().write().await;
                if storage.has_any_asset().await? {
                    warn!("No asset detected while syncing head state, deleting local assets");
                    storage.delete_assets().await?;
                    return Ok(false)
                }
            }

            trace!("found {} assets", assets.len());

            assets
        };

        trace!("assets: {}", detected_assets.len());

        // First lets add all these new assets to our DB
        stream::iter(detected_assets.into_iter().map(Ok::<_, Error>))
            .try_for_each_concurrent(self.concurrency, |asset| async move {
                trace!("asset: {}", asset);
                // check if we have this asset locally
                if !{
                    let storage = self.wallet.get_storage().read().await;
                    storage.get_asset(&asset).await.is_ok()
                } {
                    debug!("Discovered a new asset {}", asset);
                    let data = self.api.get_asset(&asset).await?;
                    // Add the asset to the storage
                    {
                        let mut storage = self.wallet.get_storage().write().await;
                        storage.add_asset(&asset, data.inner.clone()).await?;
                    }

                    // New asset detected added to the wallet, inform listeners
                    self.wallet.propagate_event(Event::NewAsset(data)).await;
                }

                Ok(())
            }).await?;

        // Now, we only sync the balances of the tracked assets
        let tracked_assets = if let Some(assets) = assets.filter(|a| !a.is_empty()) {
            let mut tracked_assets = HashSet::new();
            let storage = self.wallet.get_storage().read().await;

            for asset in assets {
                if storage.is_asset_tracked(&asset)? {
                    tracked_assets.insert(asset);
                } else {
                    debug!("Asset {} was requested but its not tracked, skipping...", asset);
                }
            }

            tracked_assets
        } else {
            // Update all tracked assets
            let storage = self.wallet.get_storage().read().await;
            let iter = storage.get_tracked_assets()?;
            let mut tracked_assets = HashSet::new();

            // Only track assets that are actually available
            for res in iter {
                let asset = res?;
                if storage.has_asset(&asset).await? {
                    tracked_assets.insert(asset);
                } else {
                    debug!("Tracked asset {} is not available", asset);
                }
            }

            tracked_assets
        };

        trace!("Tracked assets: {}", tracked_assets.len());

        let mut should_sync_blocks = stream::iter(tracked_assets.into_iter())
            .map(|asset| async move {
                trace!("requesting latest balance for {}", asset);
                // get the balance for this asset
                let result = match self.api.get_balance(&address, &asset).await {
                    Ok(res) => res,
                    Err(e) => {
                        warn!("No balance found for tracked asset {}: {}", asset, e);
                        self.wallet.propagate_event(Event::SyncError { message: format!("Error on asset {}: {}", asset, e) }).await;
                        return Ok(false)
                    }
                };
                trace!("found balance at topoheight: {}", result.topoheight);

                let mut ciphertext = result.version.take_balance();
                let topoheight = result.topoheight;
                let (must_update, balance_cache, max_supply) = {
                    let storage = self.wallet.get_storage().read().await;
                    let must_update = match storage.get_balance_for(&asset).await {
                        Ok(mut previous) => previous.ciphertext.compressed() != ciphertext.compressed(),
                        // If we don't have a balance for this asset, we should update it
                        Err(_) => true
                    };

                    // If we must update, check if we have a cache for this balance
                    let balance_cache = if must_update {
                        storage.get_unconfirmed_balance_decoded_for(&asset, &ciphertext.compressed()).await?
                    } else {
                        None
                    };

                    let max_supply = storage.get_asset(&asset).await?
                        .get_max_supply();

                    (must_update, balance_cache, max_supply)
                };

                if must_update {
                    debug!("must update balance for asset: {}, ct: {}", asset, ciphertext);
                    let value = if let Some(cache) = balance_cache {
                        cache
                    } else {
                        trace!("Decrypting balance for asset {}", asset);
                        let decompressed = ciphertext.decompressed()?;
                        match self.wallet.decrypt_ciphertext_with(decompressed.clone(), max_supply).await? {
                            Some(v) => v,
                            None => {
                                warn!("Couldn't decrypt ciphertext for asset {}, skipping it", asset);
                                return Ok::<_, Error>(false);
                            }
                        }
                    };

                    // Inform the change of the balance
                    self.wallet.propagate_event(Event::BalanceChanged(BalanceChanged {
                        asset: asset.clone(),
                        balance: value
                    })).await;

                    // Update the balance
                    let mut storage = self.wallet.get_storage().write().await;
                    debug!("Storing balance at topoheight {} for asset {} ({}) {}", topoheight, asset, value, ciphertext);
                    storage.set_balance_for(&asset, Balance::new(value, ciphertext)).await?;

                    Ok(true)
                } else {
                    debug!("balance for asset {} is already up-to-date", asset);
                    Ok(false)
                }
            })
            .buffer_unordered(self.concurrency)
            .try_fold(false, |acc, x| async move {
                Ok(acc | x)
            }).await?;

        // Apply changes
        {
            if let Some(new_nonce) = new_nonce {
                let mut storage = self.wallet.get_storage().write().await;
                if storage.get_nonce()? != new_nonce {
                    // Store the new nonce
                    debug!("Storing new nonce {}", new_nonce);
                    storage.set_nonce(new_nonce)?;
                    should_sync_blocks = true;
                }
            }
        }

        Ok(should_sync_blocks)
    }

    // Locate the highest valid topoheight we synced to, clean wallet storage
    // then sync again the head state
    async fn sync(&self, address: &Address, event: Option<NewBlockEvent>) -> Result<(), Error> {
        trace!("sync");

        // Should we sync new blocks ?
        let mut sync_new_blocks = false;

        let wallet_topoheight: u64;
        let daemon_topoheight: u64;
        let daemon_block_hash: Hash;

        // Handle the event
        if let Some(block) = event {
            trace!("new block event received");
            // We can safely handle it by hand because `locate_sync_topoheight_and_clean` secure us from being on a wrong chain
            if let Some(topoheight) = block.topoheight {
                let block_hash = block.hash.as_ref().clone();
                if let Some((assets, mut nonce)) = self.process_block(address, block, topoheight).await? {
                    debug!("We must sync head state, assets: {}, nonce: {:?}", assets.iter().map(|a| a.to_string()).collect::<Vec<String>>().join(", "), nonce);
                    {
                        let storage = self.wallet.get_storage().read().await;
                        // Verify that its a higher nonce than our locally stored
                        // Because if we are building queued transactions, it may break our queue
                        // Our we couldn't submit new txs before they get removed from mempool
                        let stored_nonce = storage.get_nonce().unwrap_or(0);
                        if nonce.is_some_and(|n| n <= stored_nonce) {
                            debug!("Nonce {:?} is lower or equal to stored nonce {}, skipping it", nonce, stored_nonce);
                            nonce = None;
                        }
                    }
                    // A change happened in this block, lets update balance and nonce
                    sync_new_blocks |= self.sync_head_state(&address, Some(assets), nonce, false, false).await?;
                }

                wallet_topoheight = topoheight;
                daemon_topoheight = topoheight;
                daemon_block_hash = block_hash;
            } else {
                // It is a block that got directly orphaned by DAG, ignore it
                warn!("Block {} is not ordered, skipping it", block.hash);
                return Ok(())
            }
        } else {
            debug!("No event received, verify that we are on the right chain");
            // First, locate the last topoheight valid for syncing
            (daemon_topoheight, daemon_block_hash, wallet_topoheight) = self.locate_sync_topoheight_and_clean().await?;
            debug!("Daemon topoheight: {}, wallet topoheight: {}", daemon_topoheight, wallet_topoheight);

            trace!("sync head state");
            // Now sync head state, this will helps us to determinate if we should sync blocks or not
            sync_new_blocks |= self.sync_head_state(&address, None, None, true, true).await?;
        }

        // Update the topoheight and block hash for wallet
        {
            trace!("updating block reference in storage");
            let mut storage = self.wallet.get_storage().write().await;
            storage.set_synced_topoheight(daemon_topoheight)?;
            storage.set_top_block_hash(&daemon_block_hash)?;
        }

        // we have something that changed, sync transactions
        // prevent a double sync head state if history scan is disabled
        if sync_new_blocks && self.wallet.get_history_scan() {
            debug!("Syncing new blocks");
            self.sync_new_blocks(address, wallet_topoheight, true).await?;
        }

        {
            debug!("Flushing storage");
            let mut storage = self.wallet.get_storage().write().await;
            storage.flush().await?;
            debug!("Flushed storage");
        }

        // Propagate the event
        self.wallet.propagate_event(Event::NewTopoHeight { topoheight: daemon_topoheight }).await;
        debug!("Synced to topoheight {}", daemon_topoheight);
        Ok(())
    }

    // Runs an infinite loop to sync on each new block added in chain
    // Because of potential forks and DAG reorg during attacks,
    // we verify the last valid topoheight where changes happened
    async fn start_syncing(self: &Arc<Self>) -> Result<(), Error> {
        debug!("Starting syncing");
        // Generate only one time the address
        let address = self.wallet.get_address();
        // Do a first sync to be up-to-date with the daemon
        self.sync(&address, None).await?;

        // Thanks to websocket, we can be notified when a new block is added in chain
        // this allows us to have a instant sync of each new block instead of polling periodically

        // Because DAG can reorder any blocks in stable height, its possible we missed some txs because they were not executed
        // when the block was added. We must check on DAG reorg for each block just to be sure
        let mut on_block_ordered = self.api.on_block_ordered_event().await?;

        // For better security, verify that an orphaned TX isn't in our ledger
        // This is rare event but may happen if someone try to do something shady
        let mut on_transaction_orphaned = self.api.on_transaction_orphaned_event().await?;

        // Track also the contract transfers to our address
        let mut on_contract_transfer = self.api.on_contract_transfer_event(address.clone()).await?;

        // Network events to detect if we are online or offline
        let mut on_connection = self.api.on_connection().await;
        let mut on_connection_lost = self.api.on_connection_lost().await;

        loop {
            select! {
                biased;
                // Wait on a new block, we don't parse the block directly as it may
                // have reorg the chain
                // Wait on a new block ordered in DAG
                res = on_block_ordered.next() => {
                    let event = res?;
                    debug!("Block ordered event {} at {}", event.block_hash, event.topoheight);
                    let topoheight = event.topoheight;
                    {
                        let mut storage = self.wallet.get_storage().write().await;
                        if let Some(hash) = storage.get_block_hash_for_topoheight(topoheight).ok() {
                            if hash != *event.block_hash {
                                warn!("DAG reorg detected at topoheight {}, deleting changes at this topoheight", topoheight);
                                storage.delete_changes_at_topoheight(topoheight)?;
                                if topoheight == 0 {
                                    debug!("Deleting all transactions due to reorg until 0");
                                    storage.delete_transactions()?;
                                } else {
                                    // Otherwise in future with millions of TXs, this may take few seconds.
                                    debug!("Deleting transactions above {} due to DAG reorg", topoheight);
                                    storage.delete_transactions_at_or_above_topoheight(topoheight)?;
                                }
                            }
                        } else {
                            debug!("No block hash found for topoheight {}, syncing block {}", topoheight, event.block_hash);
                        }
                    }

                    // TODO delete all TXs & changes at this topoheight and above
                    // We need to clean up the DB as we may have some TXs that are not executed anymore
                    // and some others that got executed

                    // Sync this block again as it may have some TXs executed
                    let block = self.api.get_block_with_txs_at_topoheight(topoheight).await?;
                    self.sync(&address, Some(block)).await?;
                },
                res = on_transaction_orphaned.next() => {
                    let event = res?;
                    debug!("on transaction orphaned event {}", event.data.hash);
                    let tx = event.data;

                    let mut storage = self.wallet.get_storage().write().await;
                    if storage.has_transaction(&tx.hash)? {
                        warn!("Transaction {} was orphaned, deleting it", tx.hash);
                        storage.delete_transaction(&tx.hash)?;
                    }

                    if storage.get_tx_cache().is_some_and(|cache| cache.last_tx_hash_created.as_ref() == Some(&tx.hash)) {
                        warn!("Transaction {} was orphaned, deleting it from cache", tx.hash);
                        storage.clear_tx_cache();
                    }
                },
                res = on_contract_transfer.next() => {
                    let event = res?;
                    debug!("on contract transfer event asset {} at topo {} {}", event.asset, event.topoheight, event.block_hash);

                    let contains_asset = {
                        let storage = self.wallet.get_storage().read().await;
                        storage.contains_asset(&event.asset).await?
                    };

                    let sync_new_blocks = self.sync_head_state(&address, Some(HashSet::from_iter([event.asset.into_owned()])), None, false, false).await?;
                    debug!("sync new blocks: {}, contains asset: {}", sync_new_blocks, contains_asset);
                    // If we already contains the asset we can sync new blocks in case we missed any
                    // If we didn't loaded it before, we have no reason to go through chain
                    // because it would have been detected earlier
                    if contains_asset && sync_new_blocks {
                        self.sync_new_blocks(&address, event.topoheight, false).await?;
                    }

                    // let mut storage = self.wallet.get_storage().write().await;
                    // TODO: we need to store this output as a transaction so it appears in history
                },
                // Detect network events
                res = on_connection.recv() => {
                    trace!("on_connection");
                    res?;
                    // We are connected again, make sure we are still up-to-date with node 
                    self.sync(&address, None).await?;

                    self.wallet.propagate_event(Event::Online).await;
                },
                res = on_connection_lost.recv() => {
                    trace!("on_connection_lost");
                    res?;
                    self.wallet.propagate_event(Event::Offline).await;
                }
            }
        }
    }

    // Sync all new blocks until the current topoheight
    // If balances is set to false, only the history will be updated
    // and not the nonce or balances
    async fn sync_new_blocks(&self, address: &Address, current_topoheight: u64, balances: bool) -> Result<(), Error> {
        debug!("Scanning history for each asset");
        // Retrieve the last transaction ID
        // We will need it to re-org all the TXs we have stored
        let (assets, last_tx_id) = {
            let storage = self.wallet.get_storage().read().await;
            let assets = storage.get_assets().await?;
            let last_tx_id = storage.get_last_transaction_id()?;
            (assets, last_tx_id)
        };

        // cache for all topoheight we already processed
        // this will prevent us to request more than one time the same topoheight
        let start = Instant::now();
        let topoheight_processed = Arc::new(Mutex::new(HashSet::new()));
        let mut highest_nonce = None;
        {
            // No async stream to preserve the orders of ALL transactions
            // If we still want to run them concurrently, what we could do is:
            // - Either reorder all TXs at the reverse txs indexes below
            // - Or memory intensive: each process block returns the list of TXs
            // and each iteration below populare a BTreeMap topoheight / list of all scanned TXs
            // so we still hold the correct holder, but this is discouraged to support low devices
            for asset in assets {
                debug!("fetch history for asset {}", asset);
                if let Err(e) = self.get_balance_and_transactions(topoheight_processed.clone(), address, &asset, current_topoheight, balances, &mut highest_nonce).await {
                    error!("Error while syncing balance for asset {}: {}", asset, e);
                    self.wallet.propagate_event(Event::SyncError { message: e.to_string() }).await;
                }
            }
        }

        // Re-org all the TXs we have stored
        {
            debug!("reverse txs indexes");
            let mut storage = self.wallet.get_storage().write().await;
            storage.reverse_transactions_indexes(last_tx_id)?;
            debug!("txs indexes reversed successfully");
        }

        self.wallet.propagate_event(Event::HistorySynced { topoheight: current_topoheight }).await;
        info!("Sync new blocks completed in {:?}", start.elapsed());

        Ok(())
    }
}