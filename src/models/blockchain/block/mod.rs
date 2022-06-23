use num_traits::{One, Zero};
use serde::{Deserialize, Serialize};
use twenty_first::{
    amount::u32s::U32s,
    shared_math::b_field_element::BFieldElement,
    util_types::mutator_set::{
        mutator_set_accumulator::MutatorSetAccumulator, mutator_set_trait::MutatorSet,
    },
};

pub mod block_body;
pub mod block_header;
pub mod block_height;
pub mod mutator_set_update;
pub mod transfer_block;

use self::{
    block_body::BlockBody, block_header::BlockHeader, mutator_set_update::MutatorSetUpdate,
    transfer_block::TransferBlock,
};
use super::digest::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Block {
    pub hash: Digest,
    pub header: BlockHeader,
    pub body: BlockBody,
}

impl From<TransferBlock> for Block {
    fn from(t_block: TransferBlock) -> Self {
        Self {
            hash: t_block.header.hash(),
            header: t_block.header,
            body: t_block.body,
        }
    }
}

impl From<Block> for TransferBlock {
    fn from(block: Block) -> Self {
        Self {
            header: block.header,
            body: block.body,
        }
    }
}

impl Block {
    pub fn genesis_block() -> Self {
        let empty_mutator = MutatorSetAccumulator::default();
        let body: BlockBody = BlockBody {
            transactions: vec![],
            next_mutator_set_accumulator: empty_mutator.clone(),
            previous_mutator_set_accumulator: empty_mutator.clone(),
            mutator_set_update: MutatorSetUpdate::default(),
            stark_proof: vec![],
        };

        // This is just the UNIX timestamp when this code was written
        let timestamp: BFieldElement = BFieldElement::new(1655916990u64);

        let header: BlockHeader = BlockHeader {
            version: BFieldElement::ring_zero(),
            height: BFieldElement::ring_zero().into(),
            mutator_set_commitment: empty_mutator.get_commitment().into(),
            prev_block_digest: Digest::default(),
            timestamp,
            nonce: [
                BFieldElement::ring_zero(),
                BFieldElement::ring_zero(),
                BFieldElement::ring_zero(),
            ],
            max_block_size: 10_000,
            proof_of_work_line: U32s::zero(),
            proof_of_work_family: U32s::zero(),
            target_difficulty: U32s::one(),
            block_body_merkle_root: body.hash(),
            uncles: vec![],
        };

        Self::new(header, body)
    }

    pub fn new(header: BlockHeader, body: BlockBody) -> Self {
        let digest = header.hash();
        Self {
            body,
            header,
            hash: digest,
        }
    }

    fn devnet_is_valid(&self) -> bool {
        // What belongs here are the things that would otherwise
        // be verified by the block validity proof.

        // 1. The transaction is valid.
        // 1'. All transactions are valid.
        // (with coinbase UTXO flag set)
        //   a) verify that MS membership proof is valid, done against `previous_mutator_set_accumulator`,
        //   b) Verify that MS removal record is valid, done against `previous_mutator_set_accumulator`,
        //   c) verify that all transactinos are represented in mutator_set_update
        //     i) Verify that all input UTXOs are present in `removals`
        //     ii) Verify that all output UTXOs are present in `additions`
        //     iii) That there are no entries in `mutator_set_update` not present in a transaction.
        //   d) verify that adding `mutator_set_update` to `previous_mutator_set_accumulator`
        //      gives `next_mutator_set_accumulator`,
        //   e) transaction timestamp <= block timestamp
        //   f) call: `transaction.devnet_is_valid()`

        // 2. accumulated proof-of-work was computed correctly
        //  - look two blocks back, take proof_of_work_line
        //  - look 1 block back, estimate proof-of-work
        //  - add -> new proof_of_work_line
        //  - look two blocks back, take proof_of_work_family
        //  - look at all uncles, estimate proof-of-work
        //  - add -> new proof_of_work_family

        // 3. variable network parameters are computed correctly
        // 3.a) target_difficulty <- pow_line
        // 3.b) max_block_size <- difference between `pow_family[n-2] - pow_line[n-2] - (pow_family[n] - pow_line[n])`

        // 4. for every uncle
        //  4.1. verify that uncle's prev_block_digest matches with parent's prev_block_digest
        //  4.2. verify that all uncles' hash are below parent's target_difficulty

        // 5. height = previous height + 1

        // 6. `block_body_merkle_root`
        // Verify that membership p
        true
    }

    pub fn is_valid(&self) -> bool {
        // check that hash is below threshold
        // TODO: Replace RHS with block `target_difficulty` from this block
        if Into::<OrderedDigest>::into(self.hash) > MOCK_BLOCK_THRESHOLD {
            return false;
        }

        // TODO: timestamp > previous and not more than 10 seconds into future

        // TODO: `block_body_merkle_root` is hash of block body.

        // Verify that STARK proof is valid
        // TODO: Add STARK verification here

        // Verify that `transactions` match
        //     pub transactions: Vec<Transaction>,
        // pub mutator_set_accumulator: MutatorSetAccumulator<Hash>,
        // pub mutator_set_update: MutatorSetUpdate,
        if !self.devnet_is_valid() {
            return false;
        }

        true
    }
}
