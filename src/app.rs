use crate::{
    APP_CHECKER_BINARY,
    utils::{BoxedSP1Prover, Shared},
};
use charms_app_runner::{AppProverInput, AppProverOutput, AppRunner};
use charms_data::{App, B32, Data, Transaction, util};
use sp1_prover::HashableKey;
use sp1_sdk::{ProverClient, SP1Proof, SP1ProofMode, SP1Stdin};
use std::{collections::BTreeMap, sync::Arc};

pub struct Prover {
    pub sp1_client: Arc<Shared<BoxedSP1Prover>>,
    pub runner: AppRunner,
}

impl Prover {
    pub fn new() -> Self {
        Self {
            sp1_client: Arc::new(Shared::new(|| {
                Box::new(ProverClient::builder().cpu().build())
            })),
            runner: AppRunner::new(),
        }
    }

    pub(crate) fn prove(
        &self,
        app_binaries: BTreeMap<B32, Vec<u8>>,
        tx: Transaction,
        app_public_inputs: BTreeMap<App, Data>,
        app_private_inputs: BTreeMap<App, Data>,
        spell_stdin: &mut SP1Stdin,
    ) -> anyhow::Result<Option<AppProverOutput>> {
        if app_binaries.is_empty() {
            return Ok(None);
        }

        let (pk, vk) = self.sp1_client.get().setup(APP_CHECKER_BINARY);
        assert_eq!(charms_client::APP_VK, vk.hash_u32());

        let app_prover_input = AppProverInput {
            app_binaries,
            tx,
            app_public_inputs,
            app_private_inputs,
        };

        let mut app_stdin = SP1Stdin::new();
        app_stdin.write_vec(util::write(&app_prover_input)?);
        let (app_proof, _) =
            self.sp1_client
                .get()
                .prove(&pk, &app_stdin, SP1ProofMode::Compressed)?;

        let SP1Proof::Compressed(compressed_proof) = app_proof.proof else {
            unreachable!()
        };
        tracing::info!("app proof generated");
        spell_stdin.write_proof(*compressed_proof, vk.vk.clone());

        let app_prover_output: AppProverOutput = util::read(app_proof.public_values.as_slice())?;

        Ok(Some(app_prover_output))
    }
}
