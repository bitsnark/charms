pub mod app;
pub mod cli;
pub mod script;
pub mod spell;
pub mod tx;
pub mod utils;

/// RISC-V binary compiled from `charms-app-checker`.
pub const APP_CHECKER_BINARY: &[u8] = include_bytes!("./bin/charms-app-checker");
/// RISC-V binary compiled from `charms-spell-checker`.
pub const SPELL_CHECKER_BINARY: &[u8] = include_bytes!("./bin/charms-spell-checker");

/// Verification key for the `charms-spell-checker` binary.
pub const SPELL_VK: &str = "0x001f3c1121c14c849b3fec7beaa93fabc68f32fda6fd929461f77f7d5ff63f7f";

#[cfg(test)]
mod test {
    use super::*;
    use crate::SPELL_VK;
    use sp1_sdk::{HashableKey, Prover, ProverClient};

    #[test]
    fn test_spell_vk() {
        let client = ProverClient::builder().cpu().build();

        let (_, vk) = client.setup(APP_CHECKER_BINARY);
        assert_eq!(charms_client::APP_VK, vk.hash_u32());

        let (_, vk) = client.setup(SPELL_CHECKER_BINARY);
        let s = vk.bytes32();
        assert_eq!(SPELL_VK, s.as_str());
    }
}
