pub mod app;
pub mod cli;
pub mod script;
pub mod spell;
pub mod tx;
pub mod utils;

pub use charms_proof_wrapper::SPELL_CHECKER_VK;

/// RISC-V binary compiled from `charms-spell-checker`.
pub const SPELL_CHECKER_BINARY: &[u8] = include_bytes!("./bin/charms-spell-checker");
/// RISC-V binary compiled from `charms-proof-wrapper`.
pub const PROOF_WRAPPER_BINARY: &[u8] = include_bytes!("./bin/charms-proof-wrapper");

#[cfg(test)]
mod test {
    use super::*;
    use charms_lib::SPELL_VK;
    use sp1_sdk::{HashableKey, Prover, ProverClient};

    #[test]
    fn test_spell_vk() {
        let client = ProverClient::builder().cpu().build();

        let (_, vk) = client.setup(PROOF_WRAPPER_BINARY);
        let s = vk.bytes32();
        assert_eq!(SPELL_VK, s.as_str());
    }
}
