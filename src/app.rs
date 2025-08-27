use crate::utils::{BoxedSP1Prover, Shared};
use charms_app_runner::AppRunner;
use std::sync::Arc;

pub struct Prover {
    pub sp1_client: Arc<Shared<BoxedSP1Prover>>,
    pub runner: AppRunner,
}
