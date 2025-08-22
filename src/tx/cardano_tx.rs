use crate::{
    spell,
    spell::{CharmsFee, Spell},
};
use anyhow::{Error, anyhow, bail};
use charms_client::{
    cardano_tx::{CardanoTx, tx_hash, tx_id},
    tx::Tx,
};
use charms_data::{TxId, UtxoId};
use cml_chain::{
    Coin, SetTransactionInput,
    address::Address,
    fees::{LinearFee, min_no_script_fee},
    plutus::PlutusData,
    transaction::{
        DatumOption, Transaction, TransactionBody, TransactionInput, TransactionOutput,
        TransactionWitnessSet,
    },
};
use std::collections::BTreeMap;

fn tx_input(ins: &[spell::Input]) -> anyhow::Result<SetTransactionInput> {
    let inputs = ins
        .iter()
        .map(|input| {
            let Some(utxo_id) = &input.utxo_id else {
                bail!("no utxo_id in spell input {:?}", &input);
            };
            let tx_input = TransactionInput::new(tx_hash(utxo_id.0), utxo_id.1.into());
            Ok(tx_input)
        })
        .collect::<Result<Vec<_>, _>>();

    Ok(inputs?.into())
}

pub const ONE_ADA: u64 = 1000000;

fn tx_output(outs: &[spell::Output]) -> anyhow::Result<Vec<TransactionOutput>> {
    outs.iter()
        .map(|output| {
            let Some(address) = output.address.as_ref() else {
                bail!("no address in spell output {:?}", &output);
            };
            let address = Address::from_bech32(address).map_err(|e| anyhow!("{}", e))?;
            let amount = output.amount.unwrap_or(ONE_ADA);
            Ok(TransactionOutput::new(address, amount.into(), None, None))
        })
        .collect()
}

pub fn from_spell(spell: &Spell) -> anyhow::Result<CardanoTx> {
    let inputs = tx_input(&spell.ins)?;
    let outputs = tx_output(&spell.outs)?;

    let fee: Coin = 0;

    let body = TransactionBody::new(inputs, outputs, fee);
    let witness_set = TransactionWitnessSet::new();

    let tx = Transaction::new(body, witness_set, true, None);

    Ok(CardanoTx(tx))
}

fn add_spell(
    tx: Transaction,
    spell_data: &[u8],
    funding_utxo: UtxoId,
    funding_utxo_value: u64,
    change_address: Address,
    prev_txs_by_id: &BTreeMap<TxId, Tx>,
) -> Vec<Transaction> {
    let tx_body = &tx.body;

    let mut tx_inputs = tx_body.inputs.to_vec();
    let orig_inputs_amount = inputs_total_amount(&tx_body.inputs, prev_txs_by_id);

    let mut tx_outputs = tx_body.outputs.clone();
    let orig_outputs_count = tx_outputs.len() as u64;
    let mut temp_tx_outputs = tx_body.outputs.clone();

    let funding_utxo_input = TransactionInput::new(tx_hash(funding_utxo.0), funding_utxo.1.into());
    tx_inputs.push(funding_utxo_input);

    let temp_data_output = change_output(spell_data, &change_address, 0u64);

    temp_tx_outputs.push(temp_data_output);

    let temp_tx_body =
        TransactionBody::new(tx_inputs.clone().into(), temp_tx_outputs, ONE_ADA.into());
    let temp_tx_witness_set = TransactionWitnessSet::new();
    let temp_tx = Transaction::new(temp_tx_body, temp_tx_witness_set, true, None);

    let min_fee_a: u64 = 44; // lovelace/byte
    let min_fee_b: u64 = 155381 + 50000; // lovelace
    let linear_fee = LinearFee::new(min_fee_a.into(), min_fee_b.into(), 0u64.into());

    let fee = min_no_script_fee(&temp_tx, &linear_fee).unwrap();

    let change = Coin::from(funding_utxo_value + orig_inputs_amount - orig_outputs_count * ONE_ADA)
        .checked_sub(fee)
        .unwrap();

    let data_output = change_output(spell_data, &change_address, change);

    tx_outputs.push(data_output);

    let tx_body = TransactionBody::new(tx_inputs.into(), tx_outputs, fee);
    let tx_witness_set = TransactionWitnessSet::new();
    let tx = Transaction::new(tx_body, tx_witness_set, true, None);

    vec![tx]
}

fn change_output(spell_data: &[u8], change_address: &Address, change: u64) -> TransactionOutput {
    TransactionOutput::new(
        change_address.clone(),
        change.into(),
        Some(DatumOption::Datum {
            datum: PlutusData::Bytes {
                bytes: spell_data.to_vec(),
                bytes_encoding: Default::default(),
            },
            len_encoding: Default::default(),
            tag_encoding: None,
            datum_tag_encoding: None,
            datum_bytes_encoding: Default::default(),
        }),
        None,
    )
}

fn inputs_total_amount(
    tx_inputs: &SetTransactionInput,
    prev_txs_by_id: &BTreeMap<TxId, Tx>,
) -> u64 {
    tx_inputs
        .iter()
        .map(|tx_input| {
            let tx_id = tx_id(tx_input.transaction_id);
            let Some(Tx::Cardano(CardanoTx(tx))) = prev_txs_by_id.get(&tx_id) else {
                unreachable!("we should already have the tx in the map")
            };
            let prev_tx_out = tx.body.outputs.get(tx_input.index as usize).unwrap();
            let amount: u64 = prev_tx_out.amount().coin.into();
            amount
        })
        .sum()
}

pub fn make_transactions(
    spell: &Spell,
    funding_utxo: UtxoId,
    funding_utxo_value: u64,
    change_address: &String,
    spell_data: &[u8],
    prev_txs_by_id: &BTreeMap<TxId, Tx>,
    _charms_fee: Option<CharmsFee>,
    _total_cycles: u64,
) -> Result<Vec<Tx>, Error> {
    let change_address =
        Address::from_bech32(change_address).map_err(|e| anyhow::anyhow!("{}", e))?;
    let tx = from_spell(spell)?;

    let transactions = add_spell(
        tx.0,
        spell_data,
        funding_utxo,
        funding_utxo_value,
        change_address,
        prev_txs_by_id,
    );
    Ok(transactions
        .into_iter()
        .map(|tx| Tx::Cardano(CardanoTx(tx)))
        .collect())
}
