use std::{
    borrow::Cow,
    collections::HashMap,
    sync::Arc
};

use anyhow::Context;
use curve25519_dalek::Scalar;
use log::{debug, trace, warn};
use indexmap::IndexMap;
use terminos_vm::{ValueCell, VM};

use crate::{
    config::{TX_GAS_BURN_PERCENT, TERMINOS_ASSET},
    contract::{ContractOutput, ContractProvider, ContractProviderWrapper},
    crypto::{elgamal::Ciphertext, Hash},
    tokio::block_in_place_safe,
    transaction::{ContractDeposit, Transaction}
};

use super::{BlockchainApplyState, BlockchainVerificationState, DecompressedDepositCt, VerificationError};

#[derive(Debug)]
pub enum InvokeContract {
    Entry(u16),
    Hook(u8),
}

impl Transaction {
    // Load and check if a contract is available
    // This is needed in case a contract has been removed or wasn't deployed due to the constructor error
    pub(super) async fn is_contract_available<'a, E, B: BlockchainVerificationState<'a, E>>(
        &'a self,
        state: &mut B,
        contract: &'a Hash,
    ) -> Result<bool, VerificationError<E>> {
        state.load_contract_module(&contract).await
            .map_err(VerificationError::State)
    }

    // Invoke a contract from a transaction
    // Note that the contract must be already loaded by calling
    // `is_contract_available`
    pub(super) async fn invoke_contract<'a, P: ContractProvider, E, B: BlockchainApplyState<'a, P, E>>(
        self: &'a Arc<Self>,
        tx_hash: &'a Hash,
        state: &mut B,
        decompressed_deposits: &HashMap<&Hash, DecompressedDepositCt>,
        contract: &'a Hash,
        deposits: &'a IndexMap<Hash, ContractDeposit>,
        parameters: impl DoubleEndedIterator<Item = ValueCell>,
        max_gas: u64,
        invoke: InvokeContract,
    ) -> Result<bool, VerificationError<E>> {
        debug!("Invoking contract {} from TX {}: {:?}", contract, tx_hash, invoke);
        let (contract_environment, mut chain_state) = state.get_contract_environment_for(contract, deposits, tx_hash).await
            .map_err(VerificationError::State)?;
    
        // Total used gas by the VM
        let (used_gas, exit_code) = block_in_place_safe::<_, Result<_, anyhow::Error>>(|| {
            // Create the VM
            let mut vm = VM::new(contract_environment.environment);

            // Insert the module to load
            vm.append_module(contract_environment.module)?;

            // Invoke the needed chunk
            // This is the first chunk to be called
            match invoke {
                InvokeContract::Entry(entry) => {
                    vm.invoke_entry_chunk(entry)
                        .context("invoke entry chunk")?;
                },
                InvokeContract::Hook(hook) => {
                    if !vm.invoke_hook_id(hook).context("invoke hook")? {
                        warn!("Invoke contract {} from TX {} hook {} not found", contract, tx_hash, hook);
                        return Ok((0, None))
                    }
                }
            }
 
            // We need to push it in reverse order because the VM will pop them in reverse order
            for constant in parameters.rev() {
                trace!("Pushing constant: {}", constant);
                vm.push_stack(constant)
                    .context("push param")?;
            }

            let context = vm.context_mut();
    
            // Set the gas limit for the VM
            context.set_gas_limit(max_gas);
    
            // Configure the context
            // Note that the VM already include the environment in Context
            context.insert_ref(self);
            // insert the chain state separetly to avoid to give the S type
            context.insert_mut(&mut chain_state);
            // insert the storage through our wrapper
            // so it can be easily mocked
            context.insert(ContractProviderWrapper(contract_environment.provider));
    
            // We need to handle the result of the VM
            let res = vm.run();
    
            // To be sure that we don't have any overflow
            // We take the minimum between the gas used and the max gas
            let gas_usage = vm.context()
                .current_gas_usage()
                .min(max_gas);
    
            let exit_code = match res {
                Ok(res) => {
                    debug!("Invoke contract {} from TX {} result: {:#}", contract, tx_hash, res);
                    // If the result return 0 as exit code, it means that everything went well
                    let exit_code = res.as_u64().ok();
                    exit_code
                },
                Err(err) => {
                    debug!("Invoke contract {} from TX {} error: {:#}", contract, tx_hash, err);
                    None
                }
            };
    
            Ok((gas_usage, exit_code))
        })?;
    
        let is_success = exit_code == Some(0);
        let mut outputs = chain_state.outputs;
        // If the contract execution was successful, we need to merge the cache
        if is_success {
            let cache = chain_state.cache;
            let tracker = chain_state.tracker;
            let assets = chain_state.assets;
            state.merge_contract_changes(
                &contract,
                cache,
                tracker,
                assets
            ).await
                .map_err(VerificationError::State)?;
        } else {
            // Otherwise, something was wrong, we delete the outputs made by the contract
            outputs.clear();
    
            if !deposits.is_empty() {
                // It was not successful, we need to refund the deposits
                self.refund_deposits(state, deposits, decompressed_deposits).await?;

                outputs.push(ContractOutput::RefundDeposits);
            }
        }

        // Push the exit code to the outputs
        outputs.push(ContractOutput::ExitCode(exit_code));

        // We must refund all the gas not used by the contract
        let refund_gas = self.handle_gas(state, used_gas, max_gas).await?;
        debug!("used gas: {}, refund gas: {}", used_gas, refund_gas);
        if refund_gas > 0 {
            outputs.push(ContractOutput::RefundGas { amount: refund_gas });
        }

        // Track the outputs
        state.set_contract_outputs(tx_hash, outputs).await
            .map_err(VerificationError::State)?;

        Ok(is_success)
    }

    pub(super) async fn handle_gas<'a, P: ContractProvider, E, B: BlockchainApplyState<'a, P, E>>(
        &'a self,
        state: &mut B,
        used_gas: u64,
        max_gas: u64
    ) -> Result<u64, VerificationError<E>> {
        // Part of the gas is burned
        let burned_gas = used_gas * TX_GAS_BURN_PERCENT / 100;
        // Part of the gas is given to the miners as fees
        let gas_fee = used_gas.checked_sub(burned_gas)
            .ok_or(VerificationError::GasOverflow)?;
        // The remaining gas is refunded to the sender
        let refund_gas = max_gas.checked_sub(used_gas)
            .ok_or(VerificationError::GasOverflow)?;

        debug!("Invoke contract used gas: {}, burned: {}, fee: {}, refund: {}", used_gas, burned_gas, gas_fee, refund_gas);
        state.add_burned_coins(burned_gas).await
            .map_err(VerificationError::State)?;

        state.add_gas_fee(gas_fee).await
            .map_err(VerificationError::State)?;

        if refund_gas > 0 {
            // If we have some funds to refund, we add it to the sender balance
            // But to prevent any front running, we add to the sender balance by considering him as a receiver.
            let balance = state.get_receiver_balance(Cow::Borrowed(self.get_source()), Cow::Owned(TERMINOS_ASSET)).await
                .map_err(VerificationError::State)?;

            *balance += Scalar::from(refund_gas);
        }

        Ok(refund_gas)
    }

    // Refund the deposits made by the user to the contract
    pub(super) async fn refund_deposits<'a, P: ContractProvider, E, B: BlockchainApplyState<'a, P, E>>(
        &'a self,
        state: &mut B,
        deposits: &'a IndexMap<Hash, ContractDeposit>,
        decompressed_deposits: &HashMap<&Hash, DecompressedDepositCt>,
    ) -> Result<(), VerificationError<E>> {
        for (asset, deposit) in deposits.iter() {
            trace!("Refunding deposit {:?} for asset: {} to {}", deposit, asset, self.source.as_address(state.is_mainnet()));

            let balance = state.get_receiver_balance(Cow::Borrowed(self.get_source()), Cow::Borrowed(asset)).await
                .map_err(VerificationError::State)?;

            match deposit {
                ContractDeposit::Public(amount) => {
                    *balance += Scalar::from(*amount);
                },
                ContractDeposit::Private { .. } => {
                    let ct = decompressed_deposits.get(asset)
                        .ok_or(VerificationError::DepositNotFound)?;

                    *balance += Ciphertext::new(ct.commitment.clone(), ct.sender_handle.clone());
                }
            }
        }

        Ok(())
    }
}