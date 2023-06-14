use anyhow::bail;
use get_size::GetSize;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use twenty_first::{
    shared_math::{b_field_element::BFieldElement, bfield_codec::BFieldCodec, tip5::Digest},
    util_types::{
        algebraic_hasher::AlgebraicHasher, merkle_tree::CpuParallel,
        merkle_tree_maker::MerkleTreeMaker,
    },
};

use super::Amount;
use crate::{
    util_types::mutator_set::{addition_record::AdditionRecord, removal_record::RemovalRecord},
    Hash,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, GetSize, BFieldCodec)]
pub struct PubScriptHashAndInput {
    pub pubscript_hash: Digest,
    pub input: Vec<BFieldElement>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, GetSize, BFieldCodec)]
pub struct TransactionKernel {
    pub inputs: Vec<RemovalRecord<Hash>>,

    // `outputs` contains the commitments (addition records) that go into the AOCL
    pub outputs: Vec<AdditionRecord>,

    pub pubscript_hashes_and_inputs: Vec<(Digest, Vec<BFieldElement>)>,
    pub fee: Amount,
    pub coinbase: Option<Amount>,

    // number of milliseconds since unix epoch
    pub timestamp: BFieldElement,

    pub mutator_set_hash: Digest,
}

impl TransactionKernel {
    pub fn mast_sequences(&self) -> Vec<Vec<BFieldElement>> {
        let input_utxos_sequence = self.inputs.encode();

        let output_utxos_sequence = self.outputs.encode();

        let pubscript_sequence = self.pubscript_hashes_and_inputs.encode();

        let fee_sequence = self.fee.encode();

        let coinbase_sequence = self.coinbase.encode();

        let timestamp_sequence = self.timestamp.encode();

        let mutator_set_hash_sequence = self.mutator_set_hash.encode();

        vec![
            input_utxos_sequence,
            output_utxos_sequence,
            pubscript_sequence,
            fee_sequence,
            coinbase_sequence,
            timestamp_sequence,
            mutator_set_hash_sequence,
        ]
    }

    pub fn mast_hash(&self) -> Digest {
        // get a sequence of BFieldElements for each field
        let mut sequences = self.mast_sequences();

        // pad until power of two
        while sequences.len() & (sequences.len() - 1) != 0 {
            sequences.push(Digest::default().encode());
        }

        // compute Merkle tree and return hash
        <CpuParallel as MerkleTreeMaker<Hash>>::from_digests(
            &sequences
                .iter()
                .map(|seq| Hash::hash_varlen(seq))
                .collect_vec(),
        )
        .get_root()
    }
}

#[cfg(test)]
pub mod transaction_kernel_tests {
    use crate::util_types::test_shared::mutator_set::*;
    use rand::{random, thread_rng, Rng, RngCore};
    use twenty_first::{amount::u32s::U32s, shared_math::other::random_elements};

    use super::*;

    pub fn random_addition_record() -> AdditionRecord {
        let ar: Digest = random();
        AdditionRecord {
            canonical_commitment: ar,
        }
    }

    pub fn random_pubscript_tuple() -> (Digest, Vec<BFieldElement>) {
        let mut rng = thread_rng();
        let digest: Digest = rng.gen();
        let len = 10 + (rng.next_u32() % 50) as usize;
        let input: Vec<BFieldElement> = random_elements(len);
        (digest, input)
    }

    pub fn random_amount() -> Amount {
        let number: [u32; 4] = random();
        Amount(U32s::new(number))
    }

    pub fn random_option<T>(thing: T) -> Option<T> {
        if thread_rng().next_u32() % 2 == 0 {
            None
        } else {
            Some(thing)
        }
    }

    pub fn random_transaction_kernel() -> TransactionKernel {
        let mut rng = thread_rng();
        let num_inputs = 1 + (rng.next_u32() % 5) as usize;
        let num_outputs = 1 + (rng.next_u32() % 6) as usize;
        let num_pubscripts = (rng.next_u32() % 5) as usize;

        let inputs = (0..num_inputs)
            .map(|_| random_removal_record())
            .collect_vec();
        let outputs = (0..num_outputs)
            .map(|_| random_addition_record())
            .collect_vec();
        let pubscripts = (0..num_pubscripts)
            .map(|_| random_pubscript_tuple())
            .collect_vec();
        let fee = random_amount();
        let coinbase = random_option(random_amount());
        let timestamp: BFieldElement = random();
        let mutator_set_hash: Digest = random();

        TransactionKernel {
            inputs,
            outputs,
            pubscript_hashes_and_inputs: pubscripts,
            fee,
            coinbase,
            timestamp,
            mutator_set_hash,
        }
    }

    #[test]
    pub fn test_decode_transaction_kernel() {
        let kernel = random_transaction_kernel();
        let encoded = kernel.encode();
        let decoded = *TransactionKernel::decode(&encoded).unwrap();
        assert_eq!(kernel, decoded);
    }
}
