use crate::prelude::twenty_first;

use twenty_first::shared_math::tip5::Digest;
use twenty_first::{
    storage::level_db::DB,
    storage::storage_schema::{traits::*, DbtSingleton, DbtVec, SimpleRustyStorage},
};

use super::monitored_utxo::MonitoredUtxo;

pub struct RustyWalletDatabase {
    storage: SimpleRustyStorage,

    monitored_utxos: DbtVec<MonitoredUtxo>,

    // records which block the database is synced to
    sync_label: DbtSingleton<Digest>,

    // counts the number of output UTXOs generated by this wallet
    counter: DbtSingleton<u64>,
}

impl RustyWalletDatabase {
    pub fn connect(db: DB) -> Self {
        let mut storage = SimpleRustyStorage::new_with_callback(
            db,
            "RustyWalletDatabase-Schema",
            crate::LOG_LOCK_EVENT_CB,
        );

        let monitored_utxos_storage = storage.schema.new_vec::<MonitoredUtxo>("monitored_utxos");
        let sync_label_storage = storage.schema.new_singleton::<Digest>("sync_label");
        let counter_storage = storage.schema.new_singleton::<u64>("counter");

        storage.restore_or_new();

        Self {
            storage,
            monitored_utxos: monitored_utxos_storage,
            sync_label: sync_label_storage,
            counter: counter_storage,
        }
    }

    /// get monitored_utxos.
    pub fn monitored_utxos(&self) -> &DbtVec<MonitoredUtxo> {
        &self.monitored_utxos
    }

    /// get mutable monitored_utxos.
    pub fn monitored_utxos_mut(&mut self) -> &mut DbtVec<MonitoredUtxo> {
        &mut self.monitored_utxos
    }

    pub fn get_sync_label(&self) -> Digest {
        self.sync_label.get()
    }

    pub fn set_sync_label(&mut self, sync_label: Digest) {
        self.sync_label.set(sync_label);
    }

    pub fn get_counter(&self) -> u64 {
        self.counter.get()
    }

    pub fn set_counter(&mut self, counter: u64) {
        self.counter.set(counter);
    }
}

impl StorageWriter for RustyWalletDatabase {
    fn persist(&mut self) {
        self.storage.persist()
    }

    fn restore_or_new(&mut self) {
        self.storage.restore_or_new()
    }
}
