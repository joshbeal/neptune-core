use get_size::GetSize;
use serde::{Deserialize, Serialize};
use tasm_lib::twenty_first::shared_math::bfield_codec::BFieldCodec;

use crate::models::consensus::mast_hash::{HasDiscriminant, MastHash};

use super::{block_body::BlockBody, block_header::BlockHeader};

/// The kernel of a block contains all data that is not proof data
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BFieldCodec, GetSize)]
pub struct BlockKernel {
    pub header: BlockHeader,
    pub body: BlockBody,
}

#[derive(Debug, Clone)]
pub enum BlockKernelField {
    Header,
    Body,
}

impl HasDiscriminant for BlockKernelField {
    fn discriminant(&self) -> usize {
        self.clone() as usize
    }
}

impl MastHash for BlockKernel {
    type FieldEnum = BlockKernelField;

    fn mast_sequences(&self) -> Vec<Vec<tasm_lib::prelude::twenty_first::prelude::BFieldElement>> {
        vec![
            self.header.mast_hash().encode(),
            self.body.mast_hash().encode(),
        ]
    }
}
