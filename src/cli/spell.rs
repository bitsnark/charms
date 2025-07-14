use crate::{
    app, cli,
    cli::{
        wallet, wallet::MIN_SATS, SpellCastParams, SpellCheckParams, SpellProveParams, BITCOIN,
        CARDANO,
    },
    spell,
    spell::{ProveRequest, ProveSpellTx, Spell},
    tx::{bitcoin_tx, cardano_tx},
    SPELL_VK,
};
use anyhow::{ensure, Error, Result};
use bitcoin::consensus::encode::deserialize_hex;
use charms_client::{tx::Tx, CURRENT_VERSION};
use charms_data::UtxoId;
use serde_json::json;
use std::{future::Future, sync::Arc};

pub trait Check {
    fn check(&self, params: SpellCheckParams) -> Result<()>;
}

pub trait Prove {
    fn prove(&self, params: SpellProveParams) -> impl Future<Output = Result<()>>;
}

pub trait Cast {
    fn cast(&self, params: SpellCastParams) -> impl Future<Output = Result<()>>;
}

pub struct SpellCli {
    pub app_prover: Arc<app::Prover>,
    pub spell_prover: Arc<spell::Prover>,
}

impl SpellCli {
    pub(crate) fn print_vk(&self) -> Result<()> {
        let json = json!({
            "version": CURRENT_VERSION,
            "vk": SPELL_VK.to_string(),
        });
        println!("{}", json);
        Ok(())
    }
}

impl Prove for SpellCli {
    async fn prove(
        &self,
        SpellProveParams {
            spell,
            prev_txs,
            app_bins,
            funding_utxo,
            funding_utxo_value,
            change_address,
            fee_rate,
            chain,
            temporary_secret_str,
        }: SpellProveParams,
    ) -> Result<()> {
        // Parse funding UTXO early: to fail fast
        let funding_utxo = UtxoId::from_str(&funding_utxo)?;

        ensure!(fee_rate >= 1.0, "fee rate must be >= 1.0");

        let spell: Spell = serde_yaml::from_slice(&std::fs::read(spell)?)?;

        let binaries = cli::app::binaries_by_vk(&self.app_prover, app_bins)?;

        let transactions = self
            .spell_prover
            .prove_spell_tx(ProveRequest {
                spell,
                binaries,
                prev_txs,
                funding_utxo,
                funding_utxo_value,
                change_address,
                fee_rate,
                charms_fee: None,
                chain: chain.clone(),
                temporary_secret_str
            })
            .await?;

        match chain.as_str() {
            BITCOIN => {
                // Convert transactions to hex and create JSON array
                let hex_txs: Vec<String> = transactions;

                // Print JSON array of transaction hexes
                println!("{}", serde_json::to_string(&hex_txs)?);
            }
            CARDANO => {
                let Some(tx_hex) = transactions.into_iter().next() else {
                    unreachable!()
                };
                let tx_draft = json!({
                    "type": "Unwitnessed Tx ConwayEra",
                    "description": "Ledger Cddl Format",
                    "cborHex": tx_hex,
                });
                println!("{}", tx_draft);
            }
            _ => unreachable!(),
        }

        Ok(())
    }
}

impl Check for SpellCli {
    #[tracing::instrument(level = "debug", skip(self, spell, app_bins))]
    fn check(
        &self,
        SpellCheckParams {
            spell,
            app_bins,
            prev_txs,
            chain,
        }: SpellCheckParams,
    ) -> Result<()> {
        let mut spell: Spell = serde_yaml::from_slice(&std::fs::read(spell)?)?;
        for u in spell.outs.iter_mut() {
            u.amount.get_or_insert(crate::cli::wallet::MIN_SATS);
        }

        // make sure spell inputs all have utxo_id
        ensure!(
            spell.ins.iter().all(|u| u.utxo_id.is_some()),
            "all spell inputs must have utxo_id"
        );

        let chain = chain.as_str();

        let tx = match chain {
            BITCOIN => Tx::Bitcoin(bitcoin_tx::from_spell(&spell)?),
            CARDANO => Tx::Cardano(cardano_tx::from_spell(&spell)?),
            _ => unreachable!(),
        };

        let prev_txs = match prev_txs {
            Some(prev_txs) => prev_txs,
            None => match tx {
                Tx::Bitcoin(tx) => cli::tx::get_prev_txs(&tx.0)?,
                Tx::Cardano(_) => unimplemented!("prev_txs required for Cardano"),
            },
        };

        let prev_txs = prev_txs
            .iter()
            .map(|tx_hex| Tx::from_hex(tx_hex))
            .collect::<Result<Vec<_>, _>>()?;

        let prev_spells = charms_client::prev_spells(&prev_txs, &SPELL_VK);

        let (norm_spell, app_private_inputs, tx_ins_beamed_source_utxos) = spell.normalized()?;

        ensure!(
            charms_client::well_formed(&norm_spell, &prev_spells, &tx_ins_beamed_source_utxos),
            "spell is not well-formed"
        );

        let binaries = cli::app::binaries_by_vk(&self.app_prover, app_bins)?;

        let charms_tx = spell.to_tx()?;
        self.app_prover.run_all(
            &binaries,
            &charms_tx,
            &norm_spell.app_public_inputs,
            &app_private_inputs,
            None,
        )?;

        Ok(())
    }
}

impl Cast for SpellCli {
    async fn cast(
        &self,
        SpellCastParams {
            spell,
            app_bins,
            funding_utxo,
            fee_rate,
            chain,
            prev_txs,
        }: SpellCastParams,
    ) -> Result<()> {
        ensure!(chain == "bitcoin", "chain must be bitcoin for now");

        let funding_utxo_str = funding_utxo.as_str();

        // Parse funding UTXO early: to fail fast
        let funding_utxo = UtxoId::from_str(&funding_utxo_str)?;
        let funding_utxo_outpoint = cli::tx::parse_outpoint(funding_utxo_str)?;

        ensure!(fee_rate >= 1.0, "fee rate must be >= 1.0");
        let mut spell: Spell = serde_yaml::from_slice(&std::fs::read(spell)?)?;

        spell_pre_checks(&spell)?;

        for u in spell.outs.iter_mut() {
            u.amount.get_or_insert(MIN_SATS);
        }

        let prev_txs = match prev_txs {
            Some(prev_txs) => prev_txs,
            None => match chain.as_str() {
                BITCOIN => gather_prev_txs(&spell)?,
                CARDANO => unimplemented!("not implemented for Cardano"),
                _ => unimplemented!(),
            },
        };

        let funding_utxo_value = wallet::funding_utxo_value(&funding_utxo_outpoint)?;
        let change_address = wallet::new_change_address()?.assume_checked().to_string();

        let binaries = cli::app::binaries_by_vk(&self.app_prover, app_bins)?;

        let txs = self
            .spell_prover
            .prove_spell_tx(ProveRequest {
                spell,
                binaries,
                prev_txs,
                funding_utxo,
                funding_utxo_value,
                change_address,
                fee_rate,
                charms_fee: None,
                chain,
                temporary_secret_str: None, // Temporary secret string is not used in casting
            })
            .await?;
        let [commit_tx, spell_tx] = txs.as_slice() else {
            unreachable!()
        };

        let signed_commit_tx_hex = wallet::sign_tx(&commit_tx)?;
        let signed_spell_tx_hex = wallet::sign_spell_tx(&spell_tx, &deserialize_hex(commit_tx)?)?;

        // Print JSON array of transaction hexes
        println!(
            "{}",
            serde_json::to_string(&[signed_commit_tx_hex, signed_spell_tx_hex])?
        );

        Ok(())
    }
}

#[tracing::instrument(level = "debug", skip(spell))]
fn gather_prev_txs(spell: &Spell) -> Result<Vec<String>, Error> {
    let tx = bitcoin_tx::from_spell(&spell)?;
    let prev_txs = cli::tx::get_prev_txs(&tx.0)?;
    Ok(prev_txs)
}

#[tracing::instrument(level = "debug", skip(spell))]
fn spell_pre_checks(spell: &Spell) -> Result<(), Error> {
    // make sure spell inputs all have utxo_id
    ensure!(
        spell.ins.iter().all(|u| u.utxo_id.is_some()),
        "all spell inputs must have utxo_id"
    );

    // make sure spell outputs all have addresses
    ensure!(
        spell.outs.iter().all(|u| u.address.is_some()),
        "all spell outputs must have addresses"
    );
    Ok(())
}
