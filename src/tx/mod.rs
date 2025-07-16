use crate::{spell::Spell, SPELL_VK};
use charms_client::{
    tx::{EnchantedTx, Tx},
    NormalizedSpell,
};
use charms_data::TxId;
use std::collections::BTreeMap;

pub mod bitcoin_tx;
pub mod cardano_tx;

#[tracing::instrument(level = "debug", skip_all)]
pub fn norm_spell(tx: &Tx) -> Option<NormalizedSpell> {
    charms_client::tx::extract_and_verify_spell(SPELL_VK, tx)
        .map_err(|e| {
            tracing::debug!("spell verification failed: {:?}", e);
            e
        })
        .ok()
}

#[tracing::instrument(level = "debug", skip_all)]
pub fn spell(tx: &Tx) -> Option<Spell> {
    match norm_spell(tx) {
        Some(norm_spell) => Some(Spell::denormalized(&norm_spell)),
        None => None,
    }
}

pub fn txs_by_txid(prev_txs: &[Tx]) -> BTreeMap<TxId, Tx> {
    prev_txs
        .iter()
        .map(|prev_tx| (prev_tx.tx_id(), prev_tx.clone()))
        .collect::<BTreeMap<_, _>>()
}
