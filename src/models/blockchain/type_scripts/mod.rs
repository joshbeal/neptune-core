use crate::{
    models::consensus::{SecretWitness, ValidationLogic},
    Hash,
};
use get_size::GetSize;
use serde::{Deserialize, Serialize};
use std::hash::{Hash as StdHash, Hasher as StdHasher};
use tasm_lib::{
    triton_vm::{instruction::LabelledInstruction, program::Program},
    twenty_first::shared_math::bfield_codec::BFieldCodec,
    Digest,
};

use native_currency::native_currency_program;

use super::transaction::primitive_witness::PrimitiveWitness;

pub mod native_currency;
pub mod neptune_coins;
pub mod time_lock;

trait TypeScriptValidationLogic<ExternalWitness>:
    ValidationLogic<(PrimitiveWitness, ExternalWitness)>
where
    ExternalWitness: BFieldCodec,
    (PrimitiveWitness, ExternalWitness): SecretWitness,
{
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, GetSize, BFieldCodec)]
pub struct TypeScript {
    pub program: Program,
}

// Standard hash needed for filtering out duplicates.
impl StdHash for TypeScript {
    fn hash<H: StdHasher>(&self, state: &mut H) {
        self.program.instructions.hash(state);
    }
}

impl From<Vec<LabelledInstruction>> for TypeScript {
    fn from(instrs: Vec<LabelledInstruction>) -> Self {
        Self {
            program: Program::new(&instrs),
        }
    }
}

impl From<&[LabelledInstruction]> for TypeScript {
    fn from(instrs: &[LabelledInstruction]) -> Self {
        Self {
            program: Program::new(instrs),
        }
    }
}

impl TypeScript {
    pub fn new(program: Program) -> Self {
        Self { program }
    }

    pub fn hash(&self) -> Digest {
        self.program.hash::<Hash>()
    }

    pub fn native_coin() -> Self {
        Self {
            program: native_currency_program(),
        }
    }
}