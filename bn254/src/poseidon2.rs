//! Diffusion matrix for Bn254
//!
//! Reference: https://github.com/HorizenLabs/poseidon2/blob/main/plain_implementations/src/poseidon2/poseidon2_instance_bn256.rs

extern crate alloc;

use alloc::vec::Vec;

use p3_field::PrimeCharacteristicRing;
use p3_poseidon2::{
    ExternalLayer, ExternalLayerConstants, ExternalLayerConstructor, HLMDSMat4, InternalLayer,
    InternalLayerConstructor, Poseidon2, add_rc_and_sbox_generic, external_initial_permute_state,
    external_terminal_permute_state, internal_permute_state,
};

use crate::Bn254;

/// Degree of the chosen permutation polynomial for BN254, used as the Poseidon2 S-Box.
///
/// As p - 1 is divisible by 2 and 3 the smallest choice for a degree D satisfying gcd(p - 1, D) = 1 is 5.
const BN254_S_BOX_DEGREE: u64 = 5;

/// An implementation of the Poseidon2 hash function for the Bn254Fr field.
///
/// It acts on arrays of the form `[Bn254Fr; WIDTH]`.
pub type Poseidon2Bn254<const WIDTH: usize> = Poseidon2<
    Bn254,
    Poseidon2ExternalLayerBn254<WIDTH>,
    Poseidon2InternalLayerBn254,
    WIDTH,
    BN254_S_BOX_DEGREE,
>;

/// Currently we only support a single width for Poseidon2 BN254.
const BN254_WIDTH: usize = 3;

#[derive(Debug, Clone, Default)]
pub struct Poseidon2InternalLayerBn254 {
    internal_constants: Vec<Bn254>,
}

impl InternalLayerConstructor<Bn254> for Poseidon2InternalLayerBn254 {
    fn new_from_constants(internal_constants: Vec<Bn254>) -> Self {
        Self { internal_constants }
    }
}

/// A faster version of `matmul_internal` making use of the fact that
/// the internal matrix is equal to:
/// ```ignore
///                             [2, 1, 1]
///     1 + Diag([1, 1, 2]) =   [1, 2, 1]
///                             [1, 1, 3]
/// ```
fn bn254_matmul_internal(state: &mut [Bn254; 3]) {
    // We bracket in this way as the s-box is applied to state[0] so this lets us
    // begin this computation before the s-box finishes.
    let sum = state[0] + (state[1] + state[2]);

    state[0] += sum;
    state[1] += sum;
    state[2] = state[2].double() + sum;
}

impl InternalLayer<Bn254, BN254_WIDTH, BN254_S_BOX_DEGREE> for Poseidon2InternalLayerBn254 {
    /// Perform the internal layers of the Poseidon2 permutation on the given state.
    fn permute_state(&self, state: &mut [Bn254; BN254_WIDTH]) {
        internal_permute_state(state, bn254_matmul_internal, &self.internal_constants)
    }
}

pub type Poseidon2ExternalLayerBn254<const WIDTH: usize> = ExternalLayerConstants<Bn254, WIDTH>;

impl<const WIDTH: usize> ExternalLayerConstructor<Bn254, WIDTH>
    for Poseidon2ExternalLayerBn254<WIDTH>
{
    fn new_from_constants(external_constants: Self) -> Self {
        external_constants
    }
}

impl<const WIDTH: usize> ExternalLayer<Bn254, WIDTH, BN254_S_BOX_DEGREE>
    for Poseidon2ExternalLayerBn254<WIDTH>
{
    /// Perform the initial external layers of the Poseidon2 permutation on the given state.
    fn permute_state_initial(&self, state: &mut [Bn254; WIDTH]) {
        external_initial_permute_state(
            state,
            self.get_initial_constants(),
            add_rc_and_sbox_generic,
            &HLMDSMat4,
        );
    }

    /// Perform the terminal external layers of the Poseidon2 permutation on the given state.
    fn permute_state_terminal(&self, state: &mut [Bn254; WIDTH]) {
        external_terminal_permute_state(
            state,
            self.get_terminal_constants(),
            add_rc_and_sbox_generic,
            &HLMDSMat4,
        );
    }
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;
    use p3_field::PrimeField;
    use p3_poseidon2::ExternalLayerConstants;
    use p3_symmetric::Permutation;
    use rand::rngs::SmallRng;
    use rand::{Rng, SeedableRng};
    use zkhash::ark_ff::{BigInteger, PrimeField as ark_PrimeField};
    use zkhash::fields::bn256::FpBN256 as ark_FpBN256;
    use zkhash::poseidon2::poseidon2::Poseidon2 as Poseidon2Ref;
    use zkhash::poseidon2::poseidon2_instance_bn256::{POSEIDON2_BN256_PARAMS, RC3};

    use super::*;
    use crate::BN254_MONTY_R_SQ;
    use crate::helpers::monty_mul;

    fn bn254_from_ark_ff(input: ark_FpBN256) -> Bn254 {
        let mut full_bytes = [0; 32];
        let bytes = input.into_bigint().to_bytes_le();
        full_bytes[..bytes.len()].copy_from_slice(&bytes);

        let value = Bn254::from_bytes_monty(&full_bytes);

        if let Some(field_elem) = value {
            // From bytes does not convert into Monty form.
            // Hence we need to do that ourselves.
            let monty_form = monty_mul(BN254_MONTY_R_SQ, field_elem.value);
            Bn254::new_monty(monty_form)
        } else {
            panic!("Invalid field element")
        }
    }

    fn ark_ff_from_bn254(input: Bn254) -> ark_FpBN256 {
        // We can't just use `input.into_bytes()` as we need to first convert out of MONTY form.
        // Going via `BigUint` is a little unnecessary but is sufficient for our purposes.
        let bigint = BigUint::from_bytes_le(&input.as_canonical_biguint().to_bytes_le());
        ark_FpBN256::from(bigint)
    }

    #[test]
    fn test_poseidon2_bn254() {
        const WIDTH: usize = 3;
        const ROUNDS_F: usize = 8;
        const ROUNDS_P: usize = 56;

        type F = Bn254;

        let mut rng = SmallRng::seed_from_u64(1);

        // Poiseidon2 reference implementation from zkhash repo.
        let poseidon2_ref = Poseidon2Ref::new(&POSEIDON2_BN256_PARAMS);

        // Copy over round constants from zkhash.
        let mut round_constants: Vec<[F; WIDTH]> = RC3
            .iter()
            .map(|vec| {
                vec.iter()
                    .copied()
                    .map(bn254_from_ark_ff)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap()
            })
            .collect();

        let internal_start = ROUNDS_F / 2;
        let internal_end = (ROUNDS_F / 2) + ROUNDS_P;
        let internal_round_constants = round_constants
            .drain(internal_start..internal_end)
            .map(|vec| vec[0])
            .collect::<Vec<_>>();
        let external_round_constants = ExternalLayerConstants::new(
            round_constants[..(ROUNDS_F / 2)].to_vec(),
            round_constants[(ROUNDS_F / 2)..].to_vec(),
        );
        // Our Poseidon2 implementation.
        let poseidon2 = Poseidon2Bn254::new(external_round_constants, internal_round_constants);

        // Generate random input and convert to both field formats.
        let input = rng.random::<[F; WIDTH]>();
        let input_ark_ff = input.map(ark_ff_from_bn254);

        // Run reference implementation.
        let output_ref: [ark_FpBN256; WIDTH] =
            poseidon2_ref.permutation(&input_ark_ff).try_into().unwrap();
        let expected: [F; WIDTH] = output_ref.map(bn254_from_ark_ff);

        // Run our implementation.
        let mut output = input;
        poseidon2.permute_mut(&mut output);

        assert_eq!(output, expected);
    }
}
