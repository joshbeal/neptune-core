use crate::models::blockchain::block::block_body::BlockBody;
use crate::models::blockchain::block::block_header::BlockHeader;
use crate::models::blockchain::block::block_height::BlockHeight;
use crate::models::blockchain::block::mutator_set_update::*;
use crate::models::blockchain::block::*;
use crate::models::blockchain::shared::*;
use crate::models::blockchain::transaction::neptune_coins::NeptuneCoins;
use crate::models::blockchain::transaction::transaction_kernel::TransactionKernel;
use crate::models::blockchain::transaction::utxo::*;
use crate::models::blockchain::transaction::validity::TransactionValidationLogic;
use crate::models::blockchain::transaction::*;
use crate::models::channel::*;
use crate::models::consensus::mast_hash::MastHash;
use crate::models::shared::SIZE_20MB_IN_BYTES;
use crate::models::state::wallet::utxo_notification_pool::{ExpectedUtxo, UtxoNotifier};
use crate::models::state::wallet::WalletSecret;
use crate::models::state::{GlobalState, GlobalStateLock};
use crate::prelude::twenty_first;
use crate::util_types::mutator_set::mutator_set_accumulator::MutatorSetAccumulator;
use crate::util_types::mutator_set::mutator_set_trait::{commit, MutatorSet};
use anyhow::{Context, Result};
use futures::channel::oneshot;
use num_traits::identities::Zero;
use rand::rngs::StdRng;
use rand::thread_rng;
use rand::Rng;
use rand::SeedableRng;
use std::ops::Deref;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tasm_lib::twenty_first::util_types::mmr::mmr_accumulator::MmrAccumulator;
use tasm_lib::twenty_first::util_types::mmr::mmr_trait::Mmr;
use tokio::select;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::*;
use twenty_first::amount::u32s::U32s;
use twenty_first::shared_math::b_field_element::BFieldElement;
use twenty_first::shared_math::bfield_codec::BFieldCodec;
use twenty_first::shared_math::digest::Digest;
use twenty_first::util_types::algebraic_hasher::AlgebraicHasher;
use twenty_first::util_types::emojihash_trait::Emojihash;

const MOCK_MAX_BLOCK_SIZE: u32 = 1_000_000;

/// Prepare a Block for mining
fn make_block_template(
    previous_block: &Block,
    transaction: Transaction,
) -> (BlockHeader, BlockBody) {
    let additions = transaction.kernel.outputs.clone();
    let removals = transaction.kernel.inputs.clone();
    let mut next_mutator_set_accumulator: MutatorSetAccumulator =
        previous_block.kernel.body.mutator_set_accumulator.clone();

    // Apply the mutator set update to the mutator set accumulator
    // This function mutates the MS accumulator that is given as argument to
    // the function such that the next mutator set accumulator is calculated.
    let mutator_set_update = MutatorSetUpdate::new(removals, additions);
    mutator_set_update
        .apply(&mut next_mutator_set_accumulator)
        .expect("Mutator set mutation must work");

    let mut block_mmra = previous_block.kernel.body.block_mmr_accumulator.clone();
    block_mmra.append(previous_block.hash());
    let block_body: BlockBody = BlockBody {
        transaction,
        mutator_set_accumulator: next_mutator_set_accumulator.clone(),
        lock_free_mmr_accumulator: MmrAccumulator::<Hash>::new(vec![]),
        block_mmr_accumulator: block_mmra,
        uncle_blocks: vec![],
    };

    let zero = BFieldElement::zero();
    let new_pow_line: U32s<5> =
        previous_block.kernel.header.proof_of_work_family + previous_block.kernel.header.difficulty;
    let next_block_height = previous_block.kernel.header.height.next();
    let mut block_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Got bad time timestamp in mining process")
        .as_millis() as u64;
    if block_timestamp < previous_block.kernel.header.timestamp.value() {
        warn!("Received block is timestamped in the future; mining on future-timestamped block.");
        block_timestamp = previous_block.kernel.header.timestamp.value() + 1;
    }
    let difficulty: U32s<5> = Block::difficulty_control(previous_block, block_timestamp);

    let block_header = BlockHeader {
        version: zero,
        height: next_block_height,
        prev_block_digest: previous_block.kernel.mast_hash(),
        timestamp: BFieldElement::new(block_timestamp),
        nonce: [zero, zero, zero],
        max_block_size: MOCK_MAX_BLOCK_SIZE,
        proof_of_work_line: new_pow_line,
        proof_of_work_family: new_pow_line,
        difficulty,
    };

    (block_header, block_body)
}

/// Attempt to mine a valid block for the network
async fn mine_block(
    mut block_header: BlockHeader,
    block_body: BlockBody,
    sender: oneshot::Sender<NewBlockFound>,
    global_state_lock: GlobalStateLock,
    coinbase_utxo_info: ExpectedUtxo,
    difficulty: U32s<5>,
) {
    info!(
        "Mining on block with {} outputs. Attempting to find block with height {}",
        block_body.transaction.kernel.outputs.len(),
        block_header.height
    );
    let threshold = Block::difficulty_to_digest_threshold(difficulty);

    // The RNG used to sample nonces must be thread-safe, which `thread_rng()` is not.
    // Solution: use `thread_rng()` to generate a seed, and generate a thread-safe RNG
    // seeded with that seed. The `thread_rng()` object is dropped immediately.
    let mut rng: StdRng = SeedableRng::from_seed(thread_rng().gen());
    let mut counter = 0;

    // Mining takes place here
    while Hash::hash(&block_header) >= threshold {
        if !global_state_lock.cli().unrestricted_mining {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // If the sender is cancelled, the parent to this thread most
        // likely received a new block, and this thread hasn't been stopped
        // yet by the operating system, although the call to abort this
        // thread *has* been made.
        if sender.is_canceled() {
            info!(
                "Abandoning mining of current block with height {}",
                block_header.height
            );
            return;
        }

        // Don't mine if we are syncing (but don't check too often)
        if counter % 100 == 0 && global_state_lock.lock(|s| s.net.syncing).await {
            return;
        } else {
            counter += 1;
        }

        block_header.nonce = rng.gen();
    }
    info!(
        "Found valid block with nonce: ({}, {}, {}).",
        block_header.nonce[0], block_header.nonce[1], block_header.nonce[2]
    );

    let new_block_info = NewBlockFound {
        block: Box::new(Block::new(block_header, block_body, None)),
        coinbase_utxo_info: Box::new(coinbase_utxo_info),
    };

    info!(
        "PoW digest of new block: {}. Threshold was: {threshold}",
        new_block_info.block.hash()
    );

    sender
        .send(new_block_info)
        .unwrap_or_else(|_| warn!("Receiver in mining loop closed prematurely"))
}

/// Return the coinbase UTXO for the receiving address and the "sender" randomness
/// used for the canonical AOCL commitment.
fn make_coinbase_transaction(
    coinbase_utxo: &Utxo,
    receiver_digest: Digest,
    wallet_secret: &WalletSecret,
    block_height: BlockHeight,
    mutator_set_accumulator: MutatorSetAccumulator,
) -> (Transaction, Digest) {
    let sender_randomness: Digest =
        wallet_secret.generate_sender_randomness(block_height, receiver_digest);

    let coinbase_amount = coinbase_utxo
        .coins
        .iter()
        .filter(|coin| coin.type_script_hash == TypeScript::native_coin().hash())
        .map(|coin| {
            *NeptuneCoins::decode(&coin.state)
                .expect("Make coinbase transaction: failed to parse coin state as amount.")
        })
        .sum();
    let coinbase_addition_record = commit(
        Hash::hash(coinbase_utxo),
        sender_randomness,
        receiver_digest,
    );

    let timestamp: BFieldElement = BFieldElement::new(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Got bad time timestamp in mining process")
            .as_millis()
            .try_into()
            .expect("Must call this function before 584 million years from genesis."),
    );

    let kernel = TransactionKernel {
        inputs: vec![],
        outputs: vec![coinbase_addition_record],
        public_announcements: vec![],
        fee: NeptuneCoins::zero(),
        timestamp,
        coinbase: Some(coinbase_amount),
        mutator_set_hash: mutator_set_accumulator.hash(),
    };

    let primitive_witness = TransactionPrimitiveWitness {
        input_utxos: vec![],
        type_scripts: vec![TypeScript::native_coin()],
        input_lock_scripts: vec![],
        lock_script_witnesses: vec![],
        input_membership_proofs: vec![],
        output_utxos: vec![coinbase_utxo.clone()],
        public_announcements: vec![],
        mutator_set_accumulator,
    };
    let validity_logic =
        TransactionValidationLogic::new_from_primitive_witness(&primitive_witness, &kernel);
    (
        Transaction {
            kernel,
            witness: TransactionWitness::ValidationLogic(validity_logic),
        },
        sender_randomness,
    )
}

/// Create the transaction that goes into the block template. The transaction is
/// built from the mempool and from the coinbase transaction. Also returns the
/// "sender randomness" used in the coinbase transaction.
fn create_block_transaction(
    latest_block: &Block,
    global_state: &GlobalState,
) -> (Transaction, ExpectedUtxo) {
    let block_capacity_for_transactions = SIZE_20MB_IN_BYTES;

    // Get most valuable transactions from mempool
    let transactions_to_include = global_state
        .mempool
        .get_transactions_for_block(block_capacity_for_transactions);

    // Build coinbase UTXO
    let transaction_fees = transactions_to_include
        .iter()
        .fold(NeptuneCoins::zero(), |acc, tx| acc + tx.kernel.fee);

    let coinbase_recipient_spending_key = global_state
        .wallet_state
        .wallet_secret
        .nth_generation_spending_key(0);
    let receiving_address = coinbase_recipient_spending_key.to_address();
    let next_block_height: BlockHeight = latest_block.kernel.header.height.next();

    let lock_script = receiving_address.lock_script();
    let coinbase_amount = Block::get_mining_reward(next_block_height) + transaction_fees;
    let coinbase_utxo = Utxo::new_native_coin(lock_script, coinbase_amount);

    let (coinbase_transaction, coinbase_sender_randomness) = make_coinbase_transaction(
        &coinbase_utxo,
        receiving_address.privacy_digest,
        &global_state.wallet_state.wallet_secret,
        next_block_height,
        latest_block.kernel.body.mutator_set_accumulator.clone(),
    );

    debug!(
        "Creating block transaction with mutator set hash: {}",
        latest_block
            .kernel
            .body
            .mutator_set_accumulator
            .hash()
            .emojihash()
    );

    // Merge incoming transactions with the coinbase transaction
    let merged_transaction = transactions_to_include
        .into_iter()
        .fold(coinbase_transaction, |acc, transaction| {
            Transaction::merge_with(acc, transaction)
        });

    let utxo_info_for_coinbase = ExpectedUtxo::new(
        coinbase_utxo,
        coinbase_sender_randomness,
        coinbase_recipient_spending_key.privacy_preimage,
        UtxoNotifier::OwnMiner,
    );

    (merged_transaction, utxo_info_for_coinbase)
}

/// Locking:
///   * acquires `global_state_lock` for write
pub async fn mine(
    mut from_main: watch::Receiver<MainToMiner>,
    to_main: mpsc::Sender<MinerToMain>,
    mut latest_block: Block,
    global_state_lock: GlobalStateLock,
) -> Result<()> {
    // Wait before starting mining thread to ensure that peers have sent us information about
    // their latest blocks. This should prevent the client from finding blocks that will later
    // be orphaned.
    const INITIAL_MINING_SLEEP_IN_SECONDS: u64 = 10;
    tokio::time::sleep(Duration::from_secs(INITIAL_MINING_SLEEP_IN_SECONDS)).await;

    let mut pause_mine = false;
    loop {
        let (worker_thread_tx, worker_thread_rx) = oneshot::channel::<NewBlockFound>();
        let miner_thread: Option<JoinHandle<()>> =
            if global_state_lock.lock(|s| s.net.syncing).await {
                info!("Not mining because we are syncing");
                global_state_lock.set_mining(false).await;
                None
            } else if pause_mine {
                info!("Not mining because mining was paused");
                global_state_lock.set_mining(false).await;
                None
            } else {
                // Build the block template and spawn the worker thread to mine on it
                let (transaction, coinbase_utxo_info) = create_block_transaction(
                    &latest_block,
                    global_state_lock.lock_guard().await.deref(),
                );
                let (block_header, block_body) = make_block_template(&latest_block, transaction);
                let miner_task = mine_block(
                    block_header,
                    block_body,
                    worker_thread_tx,
                    global_state_lock.clone(),
                    coinbase_utxo_info,
                    latest_block.kernel.header.difficulty,
                );
                global_state_lock.set_mining(true).await;
                Some(
                    tokio::task::Builder::new()
                        .name("mine_block")
                        .spawn(miner_task)?,
                )
            };

        // Await a message from either the worker thread or from the main loop
        select! {
            changed = from_main.changed() => {
                info!("Mining thread got message from main");
                if let e@Err(_) = changed {
                    return e.context("Miner failed to read from watch channel");
                }

                let main_message: MainToMiner = from_main.borrow_and_update().clone();
                match main_message {
                    MainToMiner::Shutdown => {
                        debug!("Miner shutting down.");

                        if let Some(mt) = miner_thread {
                            mt.abort();
                        }

                        break;
                    }
                    MainToMiner::NewBlock(block) => {
                        if let Some(mt) = miner_thread {
                            mt.abort();
                        }
                        latest_block = *block;
                        info!("Miner thread received {} block height {}", global_state_lock.lock(|s| s.cli().network).await, latest_block.kernel.header.height);
                    }
                    MainToMiner::Empty => (),
                    MainToMiner::ReadyToMineNextBlock => {
                        debug!("Got {:?} from `main_loop`", MainToMiner::ReadyToMineNextBlock);
                    }
                    MainToMiner::StopMining => {
                        debug!("Miner shutting down.");

                        pause_mine = true;

                        if let Some(mt) = miner_thread {
                            mt.abort();
                        }
                    }
                    MainToMiner::StartMining => {
                        debug!("Starting miner");

                        pause_mine = false;
                    }
                }
            }
            new_block_res = worker_thread_rx => {
                let new_block_info = match new_block_res {
                    Ok(res) => res,
                    Err(err) => {
                        warn!("Mining thread was cancelled prematurely. Got: {}", err);
                        continue;
                    }
                };

                debug!("Worker thread reports new block of height {}", new_block_info.block.kernel.header.height);

                // Sanity check, remove for more efficient mining.
                // The below PoW check could fail due to race conditions. So we don't panic,
                // we only ignore what the worker thread sent us.
                if !new_block_info.block.has_proof_of_work(&latest_block) {
                    error!("Own mined block did not have valid PoW Discarding.");
                }

                // The block, however, *must* be valid on other parameters. So here, we should panic
                // if it is not.
                assert!(new_block_info.block.is_valid(&latest_block), "Own mined block must be valid. Failed validity check after successful PoW check.");

                info!("Found new {} block with block height {}. Hash: {}", global_state_lock.cli().network, new_block_info.block.kernel.header.height, new_block_info.block.hash().emojihash());

                latest_block = *new_block_info.block.to_owned();
                to_main.send(MinerToMain::NewBlockFound(new_block_info)).await?;

                // Wait until `main_loop` has updated `global_state` before proceding. Otherwise, we would use
                // a deprecated version of the mempool to build the next block. We don't mark the from-main loop
                // received value as read yet as this would open up for race conditions if `main_loop` received
                // a block from a peer at the same time as this block was found.
                let _wait = from_main.changed().await;
                let msg = from_main.borrow().clone();
                debug!("Got {:?} msg from main after finding block", msg);
                if !matches!(msg, MainToMiner::ReadyToMineNextBlock) {
                    error!("Got bad message from `main_loop`: {:?}", msg);

                    // TODO: Handle this case
                    // We found a new block but the main thread updated with a block
                    // before our could be registered. We should mine on the one
                    // received from the main loop and not the one we found here.
                }
            }
        }
    }
    debug!("Miner shut down gracefully.");
    Ok(())
}

#[cfg(test)]
mod mine_loop_tests {
    use tracing_test::traced_test;

    use crate::{
        config_models::network::Network, models::state::UtxoReceiverData,
        tests::shared::get_mock_global_state,
    };

    use super::*;

    #[traced_test]
    #[tokio::test]
    async fn block_template_is_valid_test() -> Result<()> {
        // Verify that a block template made with transaction from the mempool is a valid block
        let premine_receiver_global_state_lock =
            get_mock_global_state(Network::Alpha, 2, None).await;
        let mut premine_receiver_global_state =
            premine_receiver_global_state_lock.lock_guard_mut().await;
        assert!(
            premine_receiver_global_state.mempool.is_empty(),
            "Mempool must be empty at startup"
        );

        // Verify constructed coinbase transaction and block template when mempool is empty
        let genesis_block = Block::genesis_block();
        let (transaction_empty_mempool, _coinbase_sender_randomness) =
            create_block_transaction(&genesis_block, &premine_receiver_global_state);
        assert_eq!(
            1,
            transaction_empty_mempool.kernel.outputs.len(),
            "Coinbase transaction with empty mempool must have exactly one output"
        );
        assert!(
            transaction_empty_mempool.kernel.inputs.is_empty(),
            "Coinbase transaction with empty mempool must have zero inputs"
        );
        let (block_header_template_empty_mempool, block_body_empty_mempool) =
            make_block_template(&genesis_block, transaction_empty_mempool);
        let block_template_empty_mempool = Block::new(
            block_header_template_empty_mempool,
            block_body_empty_mempool,
            None,
        );
        assert!(
            block_template_empty_mempool.is_valid(&genesis_block),
            "Block template created by miner with empty mempool must be valid"
        );

        // Add a transaction to the mempool
        let four_neptune_coins = NeptuneCoins::new(4).to_native_coins();
        let receiver_privacy_digest = Digest::default();
        let sender_randomness = Digest::default();
        let public_announcement = PublicAnnouncement::default();
        let tx_output = Utxo {
            coins: four_neptune_coins,
            lock_script_hash: LockScript::anyone_can_spend().hash(),
        };
        let tx_by_preminer = premine_receiver_global_state
            .create_transaction(
                vec![
                    (UtxoReceiverData {
                        utxo: tx_output,
                        sender_randomness,
                        receiver_privacy_digest,
                        public_announcement,
                    }),
                ],
                NeptuneCoins::new(1),
            )
            .await
            .unwrap();
        premine_receiver_global_state
            .mempool
            .insert(&tx_by_preminer);
        assert_eq!(1, premine_receiver_global_state.mempool.len());

        // Build transaction
        let (transaction_non_empty_mempool, _new_coinbase_sender_randomness) =
            create_block_transaction(&genesis_block, &premine_receiver_global_state);
        assert_eq!(
            3,
            transaction_non_empty_mempool.kernel.outputs.len(),
            "Transaction for block with non-empty mempool must contain coinbase output, send output, and change output"
        );
        assert_eq!(1, transaction_non_empty_mempool.kernel.inputs.len(), "Transaction for block with non-empty mempool must contain one input: the genesis UTXO being spent");

        // Build and verify block template
        let (block_header_template, block_body) =
            make_block_template(&genesis_block, transaction_non_empty_mempool);
        let block_template_non_empty_mempool = Block::new(block_header_template, block_body, None);
        assert!(
            block_template_non_empty_mempool.is_valid(&genesis_block),
            "Block template created by miner with non-empty mempool must be valid"
        );

        Ok(())
    }
}
