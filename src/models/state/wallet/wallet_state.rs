use anyhow::{bail, Result};
use itertools::Itertools;
use mutator_set_tf::util_types::mutator_set::mutator_set_accumulator::MutatorSetAccumulator;
use mutator_set_tf::util_types::mutator_set::mutator_set_trait::MutatorSet;
use mutator_set_tf::util_types::mutator_set::removal_record::RemovalRecord;
use num_traits::Zero;
use rusty_leveldb::DB;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex as TokioMutex, MutexGuard};
use tracing::{debug, error, info, warn};
use twenty_first::shared_math::b_field_element::BFieldElement;
use twenty_first::util_types::algebraic_hasher::AlgebraicHasher;
use twenty_first::util_types::emojihash_trait::Emojihash;
use twenty_first::util_types::storage_schema::StorageWriter;
use twenty_first::util_types::storage_vec::StorageVec;

use mutator_set_tf::util_types::mutator_set::ms_membership_proof::MsMembershipProof;
use twenty_first::shared_math::rescue_prime_digest::{Digest, DIGEST_LENGTH};

use super::rusty_wallet_database::RustyWalletDatabase;
use super::wallet_status::{WalletStatus, WalletStatusElement};
use super::WalletSecret;
use crate::config_models::data_directory::DataDirectory;
use crate::models::blockchain::block::Block;
use crate::models::blockchain::transaction::amount::Sign;
use crate::models::blockchain::transaction::utxo::Utxo;
use crate::models::blockchain::transaction::{amount::Amount, Transaction};
use crate::models::state::wallet::monitored_utxo::MonitoredUtxo;
use crate::models::state::wallet::rusty_wallet_database::BalanceUpdate;
use crate::Hash;

/// A wallet indexes its input and output UTXOs after blockhashes
/// so that one can easily roll-back. We don't want to serialize the
/// database handle, wherefore this struct exists.
#[derive(Clone)]
pub struct WalletState {
    // This value must be increased by one for each output.
    // Output counter counts number of outputs generated by this wallet. It does not matter
    // if these outputs are confirmed in a block or not. It adds one per output regardless.
    // The purpose of this value is to generate unique and deterministic entropy for each
    // new output.
    pub wallet_db: Arc<TokioMutex<RustyWalletDatabase>>,
    pub wallet_secret: WalletSecret,
    pub number_of_mps_per_utxo: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct StrongUtxoKey {
    utxo_digest: Digest,
    aocl_index: u64,
}

impl StrongUtxoKey {
    fn new(utxo_digest: Digest, aocl_index: u64) -> Self {
        Self {
            utxo_digest,
            aocl_index,
        }
    }
}

impl Debug for WalletState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletState")
            .field("wallet_secret", &self.wallet_secret)
            .finish()
    }
}

impl WalletState {
    pub async fn new_from_wallet_secret(
        data_dir: &DataDirectory,
        wallet_secret: WalletSecret,
        number_of_mps_per_utxo: usize,
    ) -> Self {
        // Create or connect to wallet block DB
        let wallet_db = DB::open(
            data_dir.wallet_database_dir_path(),
            rusty_leveldb::Options::default(),
        );

        let wallet_db = match wallet_db {
            Ok(wdb) => wdb,
            Err(err) => {
                error!("Could not open wallet database: {err:?}");
                panic!();
            }
        };

        let mut rusty_wallet_database = RustyWalletDatabase::connect(wallet_db);
        rusty_wallet_database.restore_or_new();

        let rusty_wallet_database = Arc::new(TokioMutex::new(rusty_wallet_database));

        let ret = Self {
            wallet_db: rusty_wallet_database.clone(),
            wallet_secret,
            number_of_mps_per_utxo,
        };

        // Wallet state has to be initialized with the genesis block, otherwise the outputs
        // from genesis would be unspendable. This should only be done *once* though
        {
            let mut wallet_db_lock = rusty_wallet_database.lock().await;
            if wallet_db_lock.get_sync_label() == Digest::default() {
                ret.update_wallet_state_with_new_block(
                    &Block::genesis_block(),
                    &mut wallet_db_lock,
                )
                .expect("Updating wallet state with genesis block must succeed");
            }
        }

        ret
    }
}

impl WalletState {
    pub fn update_wallet_state_with_new_block(
        &self,
        block: &Block,
        wallet_db_lock: &mut tokio::sync::MutexGuard<RustyWalletDatabase>,
    ) -> Result<()> {
        // A transaction contains a set of input and output UTXOs,
        // each of which contains an address (public key),
        let transaction: Transaction = block.body.transaction.clone();

        let my_pub_key = self.wallet_secret.get_public_key();

        let own_input_utxos: Vec<Utxo> = transaction.get_own_input_utxos(my_pub_key);

        let output_utxos_commitment_randomness: Vec<(Utxo, Digest)> =
            transaction.get_own_output_utxos_and_comrands(my_pub_key);

        // Derive the membership proofs for new input UTXOs, *and* in the process update existing membership
        // proofs with updates from this block
        // return early if balance is zero and this block does not affect our balance
        if own_input_utxos.is_empty()
            && output_utxos_commitment_randomness.is_empty()
            && wallet_db_lock.monitored_utxos.is_empty()
        {
            return Ok(());
        }

        println!("continuing in update_wallet_state_with_new_block...");
        println!("own output utxos: {:?}", output_utxos_commitment_randomness);
        let block_timestamp = Duration::from_millis(block.header.timestamp.value());
        for input_utxo in own_input_utxos.iter() {
            wallet_db_lock.balance_updates.push(BalanceUpdate {
                block: block.hash,
                timestamp: block_timestamp,
                amount: input_utxo.amount,
                sign: Sign::NonNegative,
            });
        }

        for (utxo, _ms_randomness) in output_utxos_commitment_randomness.iter() {
            wallet_db_lock.balance_updates.push(BalanceUpdate {
                block: block.hash,
                timestamp: block_timestamp,
                amount: utxo.amount,
                sign: Sign::Negative,
            });
        }

        // Find the membership proofs that were valid at the previous tip, as these all have
        // to be updated to the mutator set of the new block.
        let mut valid_membership_proofs_and_own_utxo_count: HashMap<
            StrongUtxoKey,
            (MsMembershipProof<Hash>, u64),
        > = HashMap::default();
        for i in 0..wallet_db_lock.monitored_utxos.len() {
            let monitored_utxo: MonitoredUtxo = wallet_db_lock.monitored_utxos.get(i);
            let utxo_digest = Hash::hash(&monitored_utxo.utxo);

            match monitored_utxo.get_membership_proof_for_block(&block.header.prev_block_digest) {
                Some(ms_mp) => {
                    debug!("Found valid mp for UTXO");
                    let insert_ret = valid_membership_proofs_and_own_utxo_count.insert(
                        StrongUtxoKey::new(utxo_digest, ms_mp.auth_path_aocl.leaf_index),
                        (ms_mp, i),
                    );
                    assert!(
                        insert_ret.is_none(),
                        "Strong key must be unique in wallet DB"
                    );
                }
                None => warn!(
                    "Unable to find valid membership proof for UTXO with digest {utxo_digest}"
                ),
            }
        }

        // Loop over all input UTXOs, applying all addition records. To
        // a) update all existing MS membership proofs
        // b) Register incoming transactions and derive their membership proofs
        let mut changed_mps = vec![];
        let mut msa_state: MutatorSetAccumulator<Hash> =
            block.body.previous_mutator_set_accumulator.to_owned();
        let mut removal_records = block.body.mutator_set_update.removals.clone();
        removal_records.reverse();
        let mut removal_records: Vec<&mut RemovalRecord<Hash>> =
            removal_records.iter_mut().collect::<Vec<_>>();
        for (mut addition_record, (utxo, commitment_randomness)) in block
            .body
            .mutator_set_update
            .additions
            .clone()
            .into_iter()
            .zip_eq(block.body.transaction.outputs.clone().into_iter())
        {
            {
                let utxo_digests = valid_membership_proofs_and_own_utxo_count
                    .keys()
                    .map(|key| key.utxo_digest)
                    .collect_vec();
                let res: Result<Vec<usize>, Box<dyn Error>> =
                    MsMembershipProof::batch_update_from_addition(
                        &mut valid_membership_proofs_and_own_utxo_count
                            .values_mut()
                            .map(|(mp, _index)| mp)
                            .collect_vec(),
                        &utxo_digests,
                        &mut msa_state.set_commitment,
                        &addition_record,
                    );
                match res {
                    Ok(mut indices_of_mutated_mps) => {
                        changed_mps.append(&mut indices_of_mutated_mps)
                    }
                    Err(_) => bail!("Failed to update membership proofs with addition record"),
                };
            }

            // Batch update removal records to keep them valid after next addition
            RemovalRecord::batch_update_from_addition(
                &mut removal_records,
                &mut msa_state.set_commitment,
            )
            .expect("MS removal record update from add must succeed in wallet handler");

            // If output UTXO belongs to us, add it to the list of monitored UTXOs and
            // add its membership proof to the list of managed membership proofs.
            if utxo.matches_pubkey(my_pub_key) {
                // TODO: Change this logging to use `Display` for `Amount` once functionality is merged from t-f
                info!(
                    "Received UTXO in block {}, height {}: value = {}",
                    block.hash.emojihash(),
                    block.header.height,
                    utxo.amount
                );
                let utxo_digest = Hash::hash(&utxo);
                let new_own_membership_proof =
                    msa_state.prove(&utxo_digest, &commitment_randomness, true);

                valid_membership_proofs_and_own_utxo_count.insert(
                    StrongUtxoKey::new(
                        utxo_digest,
                        new_own_membership_proof.auth_path_aocl.leaf_index,
                    ),
                    (
                        new_own_membership_proof,
                        wallet_db_lock.monitored_utxos.len(),
                    ),
                );

                // Add a new UTXO to the list of monitored UTXOs
                let mut mutxo = MonitoredUtxo::new(utxo, self.number_of_mps_per_utxo);
                mutxo.confirmed_in_block = Some((
                    block.hash,
                    Duration::from_millis(block.header.timestamp.value()),
                ));
                wallet_db_lock.monitored_utxos.push(mutxo);
            }

            // Update mutator set to bring it to the correct state for the next call to batch-update
            msa_state.add(&mut addition_record);
        }

        // sanity checks
        let mut mutxo_with_valid_mps = 0;
        for i in 0..wallet_db_lock.monitored_utxos.len() {
            let mutxo = wallet_db_lock.monitored_utxos.get(i);
            if mutxo.is_synced_to(&block.header.prev_block_digest)
                || mutxo.blockhash_to_membership_proof.is_empty()
            {
                mutxo_with_valid_mps += 1;
            }
        }
        assert_eq!(
            mutxo_with_valid_mps as usize,
            valid_membership_proofs_and_own_utxo_count.len(),
            "Monitored UTXO count must match number of managed membership proofs"
        );

        // Loop over all output UTXOs, applying all removal records
        debug!("Block has {} removal records", removal_records.len());
        debug!(
            "Transaction has {} inputs",
            block.body.transaction.inputs.len()
        );
        let mut i = 0;
        while let Some(removal_record) = removal_records.pop() {
            let res = MsMembershipProof::batch_update_from_remove(
                &mut valid_membership_proofs_and_own_utxo_count
                    .values_mut()
                    .map(|(mp, _index)| mp)
                    .collect_vec(),
                removal_record,
            );
            match res {
                Ok(mut indices_of_mutated_mps) => changed_mps.append(&mut indices_of_mutated_mps),
                Err(_) => bail!("Failed to update membership proofs with removal record"),
            };

            // Batch update removal records to keep them valid after next removal
            RemovalRecord::batch_update_from_remove(&mut removal_records, removal_record)
                .expect("MS removal record update from remove must succeed in wallet handler");

            // TODO: We mark membership proofs as spent, so they can be deleted. But
            // how do we ensure that we can recover them in case of a fork? For now we maintain
            // them even if the are spent, and then, later, we can add logic to remove these
            // membership proofs of spent UTXOs once they have been spent for M blocks.
            let input_utxo = block.body.transaction.inputs[i].utxo;
            if input_utxo.matches_pubkey(my_pub_key) {
                debug!(
                    "Discovered own input at input {}, marking UTXO as spent.",
                    i
                );

                let input_utxo_digest = Hash::hash(&input_utxo);
                let mut matching_utxo_indices: Vec<u64> = vec![];
                for j in 0..wallet_db_lock.monitored_utxos.len() {
                    if Hash::hash(&wallet_db_lock.monitored_utxos.get(j).utxo) == input_utxo_digest
                    {
                        matching_utxo_indices.push(j);
                    }
                }
                match matching_utxo_indices.len() {
                    0 => panic!(
                        "Discovered own input UTXO in block that did not match a monitored UTXO"
                    ),
                    1 => {
                        let mut mutxo =
                            wallet_db_lock.monitored_utxos.get(matching_utxo_indices[0]);
                        mutxo.spent_in_block = Some((
                            block.hash,
                            Duration::from_millis(block.header.timestamp.value()),
                        ));
                        wallet_db_lock
                            .monitored_utxos
                            .set(matching_utxo_indices[0], mutxo);
                    }
                    _n => {
                        // If we are monitoring multiple UTXOs with the same hash, we need another
                        // method to mark the correct UTXO as spent. Since we have the removal record
                        // we know the Bloom filter indices that this UTXO flips. So we can look for
                        // a membership proof in our list of monitored transactions that match those
                        // indices.
                        // This case will probably not be hit on main net.
                        warn!("We are monitoring multiple UTXOs with the same hash");
                        let mut removal_record_indices = removal_record.absolute_indices.clone();
                        removal_record_indices.sort_unstable();
                        let removal_record_indices = removal_record_indices.to_vec();
                        for matching_index in matching_utxo_indices {
                            match wallet_db_lock
                                .monitored_utxos
                                .get(matching_index)
                                .get_latest_membership_proof_entry()
                                .map(|x| x.1.cached_indices)
                            {
                                Some(indices) => match indices {
                                    Some(mut indices) => {
                                        indices.sort_unstable();
                                        if indices.to_vec() == removal_record_indices {
                                            let mut mutxo =
                                                wallet_db_lock.monitored_utxos.get(matching_index);
                                            mutxo.spent_in_block = Some((
                                                block.hash,
                                                Duration::from_millis(
                                                    block.header.timestamp.value(),
                                                ),
                                            ));
                                            wallet_db_lock
                                                .monitored_utxos
                                                .set(matching_index, mutxo);
                                            break;
                                        }
                                    }
                                    None => panic!("Unable to mark monitored UTXO as spent, as I don't know which one to mark")
                                    ,
                                },
                                None => panic!("Unable to mark monitored UTXO as spent, as I don't know which one to mark"),
                            }
                        }
                    }
                }
            }

            msa_state.remove(removal_record);
            i += 1;
        }

        // Sanity check that `msa_state` agrees with the mutator set from the applied block
        assert_eq!(
            block.body.next_mutator_set_accumulator.clone().hash(),
            msa_state.hash(),
            "Mutator set in wallet-handler must agree with that from applied block"
        );

        changed_mps.sort();
        changed_mps.dedup();
        debug!("Number of mutated membership proofs: {}", changed_mps.len());

        let num_monitored_utxos_after_block = wallet_db_lock.monitored_utxos.len();
        let mut num_unspent_utxos = 0;
        for j in 0..num_monitored_utxos_after_block {
            if wallet_db_lock
                .monitored_utxos
                .get(j)
                .spent_in_block
                .is_none()
            {
                num_unspent_utxos += 1;
            }
        }
        debug!("Number of unspent UTXOs: {}", num_unspent_utxos);

        for (
            StrongUtxoKey {
                utxo_digest,
                aocl_index: _,
            },
            (updated_ms_mp, own_utxo_index),
        ) in valid_membership_proofs_and_own_utxo_count
        {
            let mut monitored_utxo = wallet_db_lock.monitored_utxos.get(own_utxo_index);
            monitored_utxo.add_membership_proof_for_tip(block.hash, updated_ms_mp.to_owned());

            // Sanity check that membership proofs of non-spent transactions are still valid
            assert!(
                monitored_utxo.spent_in_block.is_some()
                    || msa_state.verify(&utxo_digest, &updated_ms_mp)
            );

            wallet_db_lock
                .monitored_utxos
                .set(own_utxo_index, monitored_utxo);

            // TODO: What if a newly added transaction replaces a transaction that was in another fork?
            // How do we ensure that this transaction is not counted twice?
            // One option is to only count UTXOs that are synced as valid.
            // Another option is to attempt to mark those abandoned monitored UTXOs as reorganized.
        }

        wallet_db_lock.set_sync_label(block.hash);
        wallet_db_lock.persist();

        Ok(())
    }

    pub async fn get_balance(&self) -> Amount {
        debug!("get_balance: Attempting to acquire lock on wallet DB.");

        // Limit scope of wallet DB lock to release it ASAP
        let sum: Amount = {
            // TODO: Consider using `try_lock` here to not hog the wallet_db lock
            // let mut wallet_db_l = self.wallet_db.try_lock();
            let lock = self.wallet_db.lock().await;

            let tick = SystemTime::now();

            let num_monitored_utxos = lock.monitored_utxos.len();
            let mut balance = Amount::zero();
            for i in 0..num_monitored_utxos {
                let monitored_utxo = lock.monitored_utxos.get(i);
                if monitored_utxo.spent_in_block.is_none() {
                    balance = balance + monitored_utxo.utxo.amount;
                }
            }
            debug!(
                "Computed balance of {} UTXOs in {:?}",
                num_monitored_utxos,
                tick.elapsed(),
            );
            balance
        };

        debug!("get_balance: Released wallet DB lock");
        sum
    }

    pub fn get_wallet_status_from_lock(
        &self,
        lock: &mut tokio::sync::MutexGuard<RustyWalletDatabase>,
        block: &Block,
    ) -> WalletStatus {
        let num_monitored_utxos = lock.monitored_utxos.len();
        let mut synced_unspent = vec![];
        let mut unsynced_unspent = vec![];
        let mut synced_spent = vec![];
        let mut unsynced_spent = vec![];
        for i in 0..num_monitored_utxos {
            let mutxo = lock.monitored_utxos.get(i);
            // println!("mutxo:\n{mutxo:?}");
            println!(
                "mutxo. Synced to: {}",
                mutxo
                    .get_latest_membership_proof_entry()
                    .as_ref()
                    .unwrap()
                    .0
                    .emojihash()
            );
            let utxo = mutxo.utxo;
            let spent = mutxo.spent_in_block.is_some();
            if let Some(mp) = mutxo.get_membership_proof_for_block(&block.hash) {
                if spent {
                    synced_spent.push(WalletStatusElement(mp.auth_path_aocl.leaf_index, utxo));
                } else {
                    synced_unspent.push((
                        WalletStatusElement(mp.auth_path_aocl.leaf_index, utxo),
                        mp.clone(),
                    ));
                }
            } else {
                let any_mp = &mutxo.blockhash_to_membership_proof.iter().next().unwrap().1;
                if spent {
                    unsynced_spent
                        .push(WalletStatusElement(any_mp.auth_path_aocl.leaf_index, utxo));
                } else {
                    unsynced_unspent
                        .push(WalletStatusElement(any_mp.auth_path_aocl.leaf_index, utxo));
                }
            }
        }
        WalletStatus {
            synced_unspent_amount: synced_unspent.iter().map(|x| x.0 .1.amount).sum(),
            synced_unspent,
            unsynced_unspent_amount: unsynced_unspent.iter().map(|x| x.1.amount).sum(),
            unsynced_unspent,
            synced_spent_amount: synced_spent.iter().map(|x| x.1.amount).sum(),
            synced_spent,
            unsynced_spent_amount: unsynced_spent.iter().map(|x| x.1.amount).sum(),
            unsynced_spent,
        }
    }

    /// Fetch the output counter from the database and increase the counter by one
    fn next_output_counter_from_lock(&self, db_lock: &mut MutexGuard<RustyWalletDatabase>) -> u64 {
        let current_counter: u64 = db_lock.get_counter();
        db_lock.set_counter(current_counter + 1);

        current_counter
    }

    /// Get the randomness for the next output UTXO and increment the output counter by one
    pub fn next_output_randomness_from_lock(
        &self,
        db_lock: &mut MutexGuard<RustyWalletDatabase>,
    ) -> Digest {
        let counter = self.next_output_counter_from_lock(db_lock);

        // TODO: Ugly hack used to generate a `Digest` from a `u128` here.
        // Once we've updated to twenty-first 0.2.0 or later use its `to_sequence` instead.
        let mut counter_as_digest: Vec<BFieldElement> = vec![BFieldElement::zero(); DIGEST_LENGTH];
        counter_as_digest[0] = BFieldElement::new(counter);
        let counter_as_digest: Digest = counter_as_digest.try_into().unwrap();
        let commitment_pseudo_randomness_seed = self.wallet_secret.get_commitment_randomness_seed();

        Hash::hash_pair(&counter_as_digest, &commitment_pseudo_randomness_seed)
    }

    pub fn allocate_sufficient_input_funds_from_lock(
        &self,
        lock: &mut tokio::sync::MutexGuard<RustyWalletDatabase>,
        requested_amount: Amount,
        block: &Block,
    ) -> Result<Vec<(Utxo, MsMembershipProof<Hash>)>> {
        // We only attempt to generate a transaction using those UTXOs that have up-to-date
        // membership proofs.
        let wallet_status: WalletStatus = self.get_wallet_status_from_lock(lock, block);

        // First check that we have enough. Otherwise return an error.
        if wallet_status.synced_unspent_amount < requested_amount {
            // TODO: Change this to `Display` print once available.
            bail!(
                "Insufficient synced amount to create transaction. Requested: {:?}, synced unspent amount: {:?}. Unsynced unspent amount: {:?}. Block is: {}",
                requested_amount,
                wallet_status.synced_unspent_amount, wallet_status.unsynced_unspent_amount,
                block.hash.emojihash());
        }

        let mut ret: Vec<(Utxo, MsMembershipProof<Hash>)> = vec![];
        let mut allocated_amount = Amount::zero();
        while allocated_amount < requested_amount {
            let next_elem = wallet_status.synced_unspent[ret.len()].clone();
            allocated_amount = allocated_amount + next_elem.0 .1.amount;
            ret.push((next_elem.0 .1, next_elem.1));
        }

        Ok(ret)
    }

    // Allocate sufficient UTXOs to generate a transaction. `amount` must include fees that are
    // paid in the transaction.
    pub async fn allocate_sufficient_input_funds(
        &self,
        requested_amount: Amount,
        block: &Block,
    ) -> Result<Vec<(Utxo, MsMembershipProof<Hash>)>> {
        let mut lock = self.wallet_db.lock().await;
        self.allocate_sufficient_input_funds_from_lock(&mut lock, requested_amount, block)
    }
}

#[cfg(test)]
mod wallet_state_tests {
    use crate::tests::shared::get_mock_wallet_state;

    #[tokio::test]
    async fn increase_output_counter_test() {
        // Verify that output counter is incremented when the counter value is fetched
        let wallet_state = get_mock_wallet_state(None).await;
        let mut db_lock = wallet_state.wallet_db.lock().await;
        for i in 0..12 {
            assert_eq!(
                i,
                wallet_state.next_output_counter_from_lock(&mut db_lock),
                "Output counter must match number of calls"
            );
        }
    }
}
