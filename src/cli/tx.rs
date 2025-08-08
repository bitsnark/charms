use crate::{
    cli,
    cli::{BITCOIN, CARDANO, ShowSpellParams},
    tx,
};
use anyhow::Result;
use charms_client::{bitcoin_tx::BitcoinTx, cardano_tx::CardanoTx, tx::Tx};

pub fn tx_show_spell(params: ShowSpellParams) -> Result<()> {
    let ShowSpellParams {
        chain,
        tx,
        json,
        mock,
    } = params;
    let tx = match chain.as_str() {
        BITCOIN => Tx::Bitcoin(BitcoinTx::from_hex(&tx)?),
        CARDANO => Tx::Cardano(CardanoTx::from_hex(&tx)?),
        _ => unimplemented!(),
    };

    match tx::spell(&tx, mock) {
        Some(spell) => cli::print_output(&spell, json)?,
        None => eprintln!("No spell found in the transaction"),
    }

    Ok(())
}
