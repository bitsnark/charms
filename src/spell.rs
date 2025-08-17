use crate::{
    SPELL_CHECKER_BINARY, app,
    cli::{BITCOIN, CARDANO, charms_fee_settings, prove_impl},
    tx::{bitcoin_tx, cardano_tx, txs_by_txid},
    utils,
    utils::{BoxedSP1Prover, Shared},
};
use anyhow::{anyhow, bail, ensure};
use ark_bls12_381::Bls12_381;
use ark_ec::pairing::Pairing;
use ark_ff::{Field, ToConstraintField};
use ark_groth16::{Groth16, ProvingKey};
use ark_relations::{
    lc, r1cs,
    r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError, Variable::One},
};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_snark::SNARK;
use ark_std::{
    rand::{RngCore, SeedableRng},
    test_rng,
};
use bitcoin::{Amount, Network, hashes::Hash};
use charms_app_runner::AppRunner;
use charms_client::{AppProverOutput, MOCK_SPELL_VK, bitcoin_tx::BitcoinTx, tx::Tx, well_formed};
pub use charms_client::{
    CURRENT_VERSION, NormalizedCharms, NormalizedSpell, NormalizedTransaction, Proof,
    SpellProverInput, to_tx,
};
use charms_data::{App, B32, Charms, Data, Transaction, TxId, UtxoId, util};
use charms_lib::SPELL_VK;
#[cfg(not(feature = "prover"))]
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_with::{IfIsHumanReadable, base64::Base64, serde_as};
use sha2::{Digest, Sha256};
use sp1_sdk::{SP1ProofMode, SP1Stdin};
use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
    sync::Arc,
};

/// Charm as represented in a spell.
/// Map of `$KEY: data`.
pub type KeyedCharms = BTreeMap<String, Data>;

/// UTXO as represented in a spell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Input {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utxo_id: Option<UtxoId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charms: Option<KeyedCharms>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beamed_from: Option<UtxoId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Output {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(alias = "sats", skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charms: Option<KeyedCharms>,
    #[serde(alias = "beamed_to", skip_serializing_if = "Option::is_none")]
    pub beam_to: Option<B32>,
}

/// Defines how spells are represented in their source form and in CLI outputs,
/// in both human-friendly (JSON/YAML) and machine-friendly (CBOR) formats.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Spell {
    /// Version of the protocol.
    pub version: u32,

    /// Apps used in the spell. Map of `$KEY: App`.
    /// Keys are arbitrary strings. They just need to be unique (inside the spell).
    pub apps: BTreeMap<String, App>,

    /// Public inputs to the apps for this spell. Map of `$KEY: Data`.
    #[serde(alias = "public_inputs", skip_serializing_if = "Option::is_none")]
    pub public_args: Option<BTreeMap<String, Data>>,

    /// Private inputs to the apps for this spell. Map of `$KEY: Data`.
    #[serde(alias = "private_inputs", skip_serializing_if = "Option::is_none")]
    pub private_args: Option<BTreeMap<String, Data>>,

    /// Transaction inputs.
    pub ins: Vec<Input>,
    /// Reference inputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<Input>>,
    /// Transaction outputs.
    pub outs: Vec<Output>,
}

impl Spell {
    /// New empty spell.
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION,
            apps: BTreeMap::new(),
            public_args: None,
            private_args: None,
            ins: vec![],
            refs: None,
            outs: vec![],
        }
    }

    /// Get a [`Transaction`] for the spell.
    pub fn to_tx(&self) -> anyhow::Result<Transaction> {
        let ins = self.strings_of_charms(&self.ins)?;
        let empty_vec = vec![];
        let refs = self.strings_of_charms(self.refs.as_ref().unwrap_or(&empty_vec))?;
        let outs = self
            .outs
            .iter()
            .map(|output| self.charms(&output.charms))
            .collect::<Result<_, _>>()?;

        Ok(Transaction { ins, refs, outs })
    }

    fn strings_of_charms(&self, inputs: &Vec<Input>) -> anyhow::Result<Vec<(UtxoId, Charms)>> {
        inputs
            .iter()
            .map(|input| {
                let utxo_id = input
                    .utxo_id
                    .as_ref()
                    .ok_or(anyhow!("missing input utxo_id"))?;
                let charms = self.charms(&input.charms)?;
                Ok((utxo_id.clone(), charms))
            })
            .collect::<Result<_, _>>()
    }

    fn charms(&self, charms_opt: &Option<KeyedCharms>) -> anyhow::Result<Charms> {
        charms_opt
            .as_ref()
            .ok_or(anyhow!("missing charms field"))?
            .iter()
            .map(|(k, v)| {
                let app = self.apps.get(k).ok_or(anyhow!("missing app {}", k))?;
                Ok((app.clone(), Data::from(v)))
            })
            .collect::<Result<Charms, _>>()
    }

    /// Get a [`NormalizedSpell`] and apps' private inputs for the spell.
    pub fn normalized(
        &self,
    ) -> anyhow::Result<(
        NormalizedSpell,
        BTreeMap<App, Data>,
        BTreeMap<UtxoId, UtxoId>,
    )> {
        ensure!(self.version == CURRENT_VERSION);

        let empty_map = BTreeMap::new();
        let keyed_public_inputs = self.public_args.as_ref().unwrap_or(&empty_map);

        let keyed_apps = &self.apps;
        let apps: BTreeSet<App> = keyed_apps.values().cloned().collect();
        let app_to_index: BTreeMap<App, u32> = apps.iter().cloned().zip(0..).collect();
        ensure!(apps.len() == keyed_apps.len(), "duplicate apps");

        let app_public_inputs: BTreeMap<App, Data> = app_inputs(keyed_apps, keyed_public_inputs);

        let ins: Vec<UtxoId> = self
            .ins
            .iter()
            .map(|utxo| utxo.utxo_id.clone().ok_or(anyhow!("missing input utxo_id")))
            .collect::<Result<_, _>>()?;
        ensure!(
            ins.iter().collect::<BTreeSet<_>>().len() == ins.len(),
            "duplicate inputs"
        );
        let ins = Some(ins);

        let refs = self
            .refs
            .as_ref()
            .map(|refs| {
                refs.iter()
                    .map(|utxo| utxo.utxo_id.clone().ok_or(anyhow!("missing input utxo_id")))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        let empty_charm = KeyedCharms::new();

        let outs: Vec<NormalizedCharms> = self
            .outs
            .iter()
            .map(|utxo| {
                let n_charms = utxo
                    .charms
                    .as_ref()
                    .unwrap_or(&empty_charm)
                    .iter()
                    .map(|(k, v)| {
                        let app = keyed_apps.get(k).ok_or(anyhow!("missing app key"))?;
                        let i = *app_to_index
                            .get(app)
                            .ok_or(anyhow!("app is expected to be in app_to_index"))?;
                        Ok((i, v.clone()))
                    })
                    .collect::<anyhow::Result<NormalizedCharms>>()?;
                Ok(n_charms)
            })
            .collect::<anyhow::Result<_>>()?;

        let beamed_outs: BTreeMap<_, _> = self
            .outs
            .iter()
            .zip(0u32..)
            .filter_map(|(o, i)| o.beam_to.as_ref().map(|b32| (i, b32.clone())))
            .collect();
        let beamed_outs = Some(beamed_outs).filter(|m| !m.is_empty());

        let norm_spell = NormalizedSpell {
            version: self.version,
            tx: NormalizedTransaction {
                ins,
                refs,
                outs,
                beamed_outs,
            },
            app_public_inputs,
            mock: false,
        };

        let keyed_private_inputs = self.private_args.as_ref().unwrap_or(&empty_map);
        let app_private_inputs = app_inputs(keyed_apps, keyed_private_inputs);

        let tx_ins_beamed_source_utxos = self
            .ins
            .iter()
            .filter_map(|input| {
                let tx_in = input
                    .utxo_id
                    .as_ref()
                    .expect("inputs are expected to have utxo_id set")
                    .clone();
                input
                    .beamed_from
                    .as_ref()
                    .map(|beam_source_utxo_id| (tx_in, beam_source_utxo_id.clone()))
            })
            .collect();

        Ok((norm_spell, app_private_inputs, tx_ins_beamed_source_utxos))
    }

    /// De-normalize a normalized spell.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn denormalized(norm_spell: &NormalizedSpell) -> anyhow::Result<Self> {
        let apps = (0..)
            .zip(norm_spell.app_public_inputs.keys())
            .map(|(i, app)| (utils::str_index(&i), app.clone()))
            .collect();

        let public_inputs = match norm_spell
            .app_public_inputs
            .values()
            .enumerate()
            .filter_map(|(i, data)| match data {
                data if data.is_empty() => None,
                data => Some((utils::str_index(&(i as u32)), data.clone())),
            })
            .collect::<BTreeMap<_, _>>()
        {
            map if map.is_empty() => None,
            map => Some(map),
        };

        let Some(norm_spell_ins) = &norm_spell.tx.ins else {
            bail!("spell must have inputs");
        };
        let ins = norm_spell_ins
            .iter()
            .map(|utxo_id| Input {
                utxo_id: Some(utxo_id.clone()),
                charms: None,
                beamed_from: None,
            })
            .collect();

        let refs = norm_spell.tx.refs.as_ref().map(|refs| {
            refs.iter()
                .map(|utxo_id| Input {
                    utxo_id: Some(utxo_id.clone()),
                    charms: None,
                    beamed_from: None,
                })
                .collect::<Vec<_>>()
        });

        let outs = norm_spell
            .tx
            .outs
            .iter()
            .zip(0u32..)
            .map(|(n_charms, i)| Output {
                address: None,
                amount: None,
                charms: match n_charms
                    .iter()
                    .map(|(i, data)| (utils::str_index(i), data.clone()))
                    .collect::<KeyedCharms>()
                {
                    charms if charms.is_empty() => None,
                    charms => Some(charms),
                },
                beam_to: norm_spell
                    .tx
                    .beamed_outs
                    .as_ref()
                    .and_then(|beamed_to| beamed_to.get(&i).cloned()),
            })
            .collect();

        Ok(Self {
            version: norm_spell.version,
            apps,
            public_args: public_inputs,
            private_args: None,
            ins,
            refs,
            outs,
        })
    }
}

fn app_inputs(
    keyed_apps: &BTreeMap<String, App>,
    keyed_inputs: &BTreeMap<String, Data>,
) -> BTreeMap<App, Data> {
    keyed_apps
        .iter()
        .map(|(k, app)| {
            (
                app.clone(),
                keyed_inputs.get(k).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

pub trait Prove: Send + Sync {
    /// Prove the correctness of a spell, generate the proof.
    ///
    /// This function generates a proof that a spell (`NormalizedSpell`) is correct.
    /// It processes application binaries, private inputs,
    /// previous transactions, and input/output mappings, and finally generates a proof
    /// of correctness for the given spell. Additionally, it calculates the
    /// cycles consumed during the process if applicable.
    ///
    /// # Parameters
    /// - `norm_spell`: A `NormalizedSpell` object representing the normalized spell that needs to
    ///   be proven.
    /// - `app_binaries`: A map containing application VKs (`B32`) as keys and their binaries as
    ///   values.
    /// - `app_private_inputs`: A map of application-specific private inputs, containing `App` keys
    ///   and associated `Data` values.
    /// - `prev_txs`: A list of previous transactions (`Tx`) that have created the outputs consumed
    ///   by the spell.
    /// - `tx_ins_beamed_source_utxos`: A mapping of input UTXOs to their beaming source UTXOs (if
    ///   the input UTXO has been beamed from another chain).
    /// - `expected_cycles`: An optional vector of cycles (`u64`) that represents the desired
    ///   execution cycles or constraints for the proof. If `None`, no specific cycle limit is
    ///   applied.
    ///
    /// # Returns
    /// - `Ok((NormalizedSpell, Proof, u64))`: On success, returns a tuple containing:
    ///   * The original `NormalizedSpell` object that was proven in its onchain form (i.e. without
    ///     the inputs, since they are already specified by the transaction).
    ///   * The generated `Proof` object, which provides evidence of correctness for the spell.
    ///   * A `u64` value indicating the total number of cycles consumed during the proving process.
    /// - `Err(anyhow::Error)`: Returns an error if the proving process fails due to validation
    ///   issues, computation errors, or other runtime problems.
    ///
    /// # Errors
    /// The function will return an error if:
    /// - Validation of the `NormalizedSpell` or its components fails.
    /// - The proof generation process encounters computation errors.
    /// - Any of the dependent data (e.g., transactions, binaries, private inputs) is inconsistent,
    ///   invalid, or missing required information.
    /// ```
    fn prove(
        &self,
        norm_spell: NormalizedSpell,
        app_binaries: BTreeMap<B32, Vec<u8>>,
        app_private_inputs: BTreeMap<App, Data>,
        prev_txs: Vec<Tx>,
        tx_ins_beamed_source_utxos: BTreeMap<UtxoId, UtxoId>,
    ) -> anyhow::Result<(NormalizedSpell, Proof, u64)>;
}

impl Prove for Prover {
    fn prove(
        &self,
        norm_spell: NormalizedSpell,
        app_binaries: BTreeMap<B32, Vec<u8>>,
        app_private_inputs: BTreeMap<App, Data>,
        prev_txs: Vec<Tx>,
        tx_ins_beamed_source_utxos: BTreeMap<UtxoId, UtxoId>,
    ) -> anyhow::Result<(NormalizedSpell, Proof, u64)> {
        ensure!(
            !norm_spell.mock,
            "trying to prove a mock spell with a real prover"
        );

        let mut stdin = SP1Stdin::new();

        let prev_spells = charms_client::prev_spells(&prev_txs, SPELL_VK, false);
        let tx = to_tx(&norm_spell, &prev_spells, &tx_ins_beamed_source_utxos);

        let app_prover_output = self.app_prover.prove(
            app_binaries,
            tx,
            norm_spell.app_public_inputs.clone(),
            app_private_inputs,
            &mut stdin,
        )?;

        let app_cycles = app_prover_output
            .as_ref()
            .map(|o| o.cycles.iter().sum())
            .unwrap_or(0);

        let prover_input = SpellProverInput {
            self_spell_vk: SPELL_VK.to_string(),
            prev_txs,
            spell: norm_spell.clone(),
            tx_ins_beamed_source_utxos,
            app_prover_output,
        };

        stdin.write_vec(util::write(&prover_input)?);

        let (pk, _) = self.prover_client.get().setup(SPELL_CHECKER_BINARY);
        let (proof, spell_cycles) =
            self.prover_client
                .get()
                .prove(&pk, &stdin, SP1ProofMode::Groth16)?;
        let proof = proof.bytes();

        let norm_spell = clear_inputs(norm_spell);

        // TODO app_cycles might turn out to be much more expensive than spell_cycles
        Ok((norm_spell, proof, app_cycles + spell_cycles))
    }
}

fn make_mock(mut norm_spell: NormalizedSpell) -> NormalizedSpell {
    norm_spell.mock = true;
    norm_spell
}

fn clear_inputs(mut norm_spell: NormalizedSpell) -> NormalizedSpell {
    norm_spell.tx.ins = None;
    norm_spell
}

impl Prove for MockProver {
    fn prove(
        &self,
        norm_spell: NormalizedSpell,
        app_binaries: BTreeMap<B32, Vec<u8>>,
        app_private_inputs: BTreeMap<App, Data>,
        prev_txs: Vec<Tx>,
        tx_ins_beamed_source_utxos: BTreeMap<UtxoId, UtxoId>,
    ) -> anyhow::Result<(NormalizedSpell, Proof, u64)> {
        let norm_spell = make_mock(norm_spell);

        let prev_spells = charms_client::prev_spells(&prev_txs, SPELL_VK, true);

        let app_prover_output = match app_binaries.is_empty() {
            true => None,
            false => {
                let tx = to_tx(&norm_spell, &prev_spells, &tx_ins_beamed_source_utxos);
                // prove charms-app-checker run
                let cycles = self.app_runner.run_all(
                    &app_binaries,
                    &tx,
                    &norm_spell.app_public_inputs,
                    &app_private_inputs,
                )?;
                Some(AppProverOutput {
                    tx,
                    app_public_inputs: norm_spell.app_public_inputs.clone(),
                    cycles,
                })
            }
        };

        let app_cycles = app_prover_output
            .as_ref()
            .map(|o| o.cycles.iter().sum())
            .unwrap_or(0);

        // prove charms-spell-checker run
        ensure!(
            charms_client::well_formed(&norm_spell, &prev_spells, &tx_ins_beamed_source_utxos),
            "spell is not well-formed"
        );

        // replace with good randomness in non-mock mode
        let mut rng = ark_std::rand::rngs::StdRng::seed_from_u64(test_rng().next_u64());

        // Create parameters for our circuit
        let pk = load_pk()?;

        let committed_data = util::write(&(MOCK_SPELL_VK, norm_spell.clone()))?;

        let field_elements = Sha256::digest(&committed_data)
            .to_field_elements()
            .expect("non-empty vector is expected");
        let circuit = DummyCircuit {
            a: Some(field_elements[0]),
        };

        let proof = Groth16::<Bls12_381>::prove(&pk, circuit, &mut rng)?;
        let mut proof_bytes = vec![];
        proof.serialize_compressed(&mut proof_bytes)?;

        let (proof, spell_cycles) = (proof_bytes, 0);

        let norm_spell = clear_inputs(norm_spell);

        Ok((norm_spell, proof, app_cycles + spell_cycles))
    }
}

fn load_pk<E: Pairing>() -> anyhow::Result<ProvingKey<E>> {
    ProvingKey::deserialize_compressed(MOCK_GROTH16_PK)
        .map_err(|e| anyhow!("Failed to deserialize proving key: {}", e))
}

const MOCK_GROTH16_PK: &[u8] = include_bytes!("./bin/mock-groth16-pk.bin");

#[derive(Default)]
pub struct DummyCircuit<F>
where
    F: Field,
{
    a: Option<F>,
}

impl<ConstraintF> ConstraintSynthesizer<ConstraintF> for DummyCircuit<ConstraintF>
where
    ConstraintF: Field,
{
    fn generate_constraints(self, cs: ConstraintSystemRef<ConstraintF>) -> r1cs::Result<()> {
        let a = cs.new_witness_variable(|| self.a.ok_or(SynthesisError::AssignmentMissing))?;
        let c = cs.new_input_variable(|| self.a.ok_or(SynthesisError::AssignmentMissing))?;
        cs.enforce_constraint(lc!() + a, lc!() + One, lc!() + c)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn deserialize_keyed_charm() {
        let y = r#"
$TOAD_SUB: 10
$TOAD: 9
"#;

        let charms: KeyedCharms = serde_yaml::from_str(y).unwrap();
        dbg!(&charms);

        let utxo_id_0 =
            UtxoId::from_str("f72700ac56bd4dd61f2ccb4acdf21d0b11bb294fc3efa9012b77903932197d2f:2")
                .unwrap();
        let buf = util::write(&utxo_id_0).unwrap();

        let utxo_id_data: Data = util::read(buf.as_slice()).unwrap();

        let utxo_id: UtxoId = utxo_id_data.value().unwrap();
        assert_eq!(utxo_id_0, dbg!(utxo_id));
    }
}

pub trait ProveSpellTx: Send + Sync {
    fn new(mock: bool) -> Self;

    fn prove_spell_tx(
        &self,
        prove_request: ProveRequest,
    ) -> impl Future<Output = anyhow::Result<Vec<String>>>;
}

pub struct ProveSpellTxImpl {
    pub mock: bool,

    pub charms_fee_settings: Option<CharmsFee>,
    pub charms_prove_api_url: String,

    pub prover: Box<dyn Prove>,
    #[cfg(not(feature = "prover"))]
    pub client: Client,
}

pub type FeeAddressForNetwork = BTreeMap<String, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CharmsFee {
    /// Fee addresses for each chain (bitcoin, cardano, etc.) further broken down by network
    /// (mainnet, testnet, etc.).
    pub fee_addresses: BTreeMap<String, FeeAddressForNetwork>,
    /// Fee rate in sats per mega cycle.
    pub fee_rate: u64,
    /// Base fee in sats.
    pub fee_base: u64,
}

impl CharmsFee {
    pub fn fee_address(&self, chain: &str, network: &str) -> Option<&str> {
        self.fee_addresses.get(chain).and_then(|fee_addresses| {
            fee_addresses
                .get(network)
                .map(|fee_address| fee_address.as_str())
        })
    }
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize)]
pub struct ProveRequest {
    pub spell: Spell,
    #[serde_as(as = "IfIsHumanReadable<BTreeMap<_, Base64>>")]
    pub binaries: BTreeMap<B32, Vec<u8>>,
    pub prev_txs: Vec<String>,
    pub funding_utxo: UtxoId,
    pub funding_utxo_value: u64,
    pub change_address: String,
    pub fee_rate: f64,
    pub chain: String,
}

pub struct Prover {
    pub app_prover: Arc<app::Prover>,
    pub prover_client: Arc<Shared<BoxedSP1Prover>>,
}

pub struct MockProver {
    pub app_runner: Arc<AppRunner>,
}

impl ProveSpellTxImpl {
    async fn do_prove_spell_tx(&self, prove_request: ProveRequest) -> anyhow::Result<Vec<String>> {
        let total_app_cycles = self.validate_prove_request(&prove_request)?;
        let ProveRequest {
            spell,
            binaries,
            prev_txs,
            funding_utxo,
            funding_utxo_value,
            change_address,
            fee_rate,
            chain,
        } = prove_request;

        let prev_txs = from_hex_txs(&prev_txs)?;
        let prev_txs_by_id = txs_by_txid(&prev_txs);

        let (norm_spell, app_private_inputs, tx_ins_beamed_source_utxos) = spell.normalized()?;

        let (norm_spell, proof, proof_app_cycles) = self.prover.prove(
            norm_spell,
            binaries,
            app_private_inputs,
            prev_txs,
            tx_ins_beamed_source_utxos,
        )?;

        let total_cycles = if !self.mock {
            total_app_cycles
        } else {
            proof_app_cycles // mock prover computes app run cycles
        };

        tracing::info!("proof generated. total app cycles: {}", total_cycles);

        // Serialize spell into CBOR
        let spell_data = util::write(&(&norm_spell, &proof))?;

        let charms_fee = self.charms_fee_settings.clone();

        match chain.as_str() {
            BITCOIN => {
                let txs = bitcoin_tx::make_transactions(
                    &spell,
                    funding_utxo,
                    funding_utxo_value,
                    &change_address,
                    &prev_txs_by_id,
                    &spell_data,
                    fee_rate,
                    charms_fee,
                    total_cycles,
                )?;
                Ok(to_hex_txs(&txs))
            }
            CARDANO => {
                let txs = cardano_tx::make_transactions(
                    &spell,
                    funding_utxo,
                    funding_utxo_value,
                    &change_address,
                    &spell_data,
                    &prev_txs_by_id,
                    charms_fee,
                    total_cycles,
                )?;
                Ok(to_hex_txs(&txs))
            }
            _ => bail!("unsupported chain: {}", chain),
        }
    }
}

impl ProveSpellTx for ProveSpellTxImpl {
    #[tracing::instrument(level = "debug")]
    fn new(mock: bool) -> Self {
        let charms_fee_settings = charms_fee_settings();

        let charms_prove_api_url = std::env::var("CHARMS_PROVE_API_URL")
            .ok()
            .unwrap_or("https://prove.charms.dev/spells/prove".to_string());

        let prover = prove_impl(mock);

        #[cfg(not(feature = "prover"))]
        let client = Client::builder()
            .use_rustls_tls() // avoids system OpenSSL issues
            .http2_prior_knowledge()
            .http2_adaptive_window(true)
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("HTTP client should be created successfully");

        Self {
            mock,
            charms_fee_settings,
            charms_prove_api_url,
            prover,
            #[cfg(not(feature = "prover"))]
            client,
        }
    }

    #[cfg(feature = "prover")]
    async fn prove_spell_tx(&self, prove_request: ProveRequest) -> anyhow::Result<Vec<String>> {
        self.do_prove_spell_tx(prove_request).await
    }

    #[cfg(not(feature = "prover"))]
    #[tracing::instrument(level = "info", skip_all)]
    async fn prove_spell_tx(&self, prove_request: ProveRequest) -> anyhow::Result<Vec<String>> {
        if self.mock {
            return Self::do_prove_spell_tx(self, prove_request).await;
        }

        self.validate_prove_request(&prove_request)?;
        let response = self
            .client
            .post(&self.charms_prove_api_url)
            .json(&prove_request)
            .send()
            .await?;
        let txs: Vec<String> = response.json().await?;
        Ok(txs)
    }
}

fn ensure_all_prev_txs_are_present(
    spell: &NormalizedSpell,
    tx_ins_beamed_source_utxos: &BTreeMap<UtxoId, UtxoId>,
    prev_txs_by_id: &BTreeMap<TxId, Tx>,
) -> anyhow::Result<()> {
    ensure!(spell.tx.ins.as_ref().is_some_and(|ins| {
        ins.iter()
            .all(|utxo_id| prev_txs_by_id.contains_key(&utxo_id.0))
    }));
    ensure!(spell.tx.refs.as_ref().is_none_or(|ins| {
        ins.iter()
            .all(|utxo_id| prev_txs_by_id.contains_key(&utxo_id.0))
    }));
    ensure!(
        tx_ins_beamed_source_utxos
            .iter()
            .all(|(utxo_id, beaming_source_utxo_id)| {
                prev_txs_by_id.contains_key(&utxo_id.0)
                    && prev_txs_by_id.contains_key(&beaming_source_utxo_id.0)
            })
    );
    Ok(())
}

impl ProveSpellTxImpl {
    pub fn validate_prove_request(&self, prove_request: &ProveRequest) -> anyhow::Result<u64> {
        let prev_txs = &prove_request.prev_txs;
        let prev_txs = from_hex_txs(&prev_txs)?;
        let prev_txs_by_id = txs_by_txid(&prev_txs);

        let (norm_spell, app_private_inputs, tx_ins_beamed_source_utxos) =
            prove_request.spell.normalized()?;
        ensure_all_prev_txs_are_present(&norm_spell, &tx_ins_beamed_source_utxos, &prev_txs_by_id)?;

        let prev_spells = charms_client::prev_spells(&prev_txs, SPELL_VK, self.mock);

        let tx = to_tx(&norm_spell, &prev_spells, &tx_ins_beamed_source_utxos);
        // prove charms-app-checker run
        let cycles = AppRunner::new(true).run_all(
            &prove_request.binaries,
            &tx,
            &norm_spell.app_public_inputs,
            &app_private_inputs,
        )?;
        let total_cycles = cycles.iter().sum();
        ensure!(well_formed(
            &norm_spell,
            &prev_spells,
            &tx_ins_beamed_source_utxos
        ));

        match prove_request.chain.as_str() {
            BITCOIN => {
                let change_address = bitcoin::Address::from_str(&prove_request.change_address)?;

                let network = match &change_address {
                    a if a.is_valid_for_network(Network::Bitcoin) => Network::Bitcoin,
                    a if a.is_valid_for_network(Network::Testnet4) => Network::Testnet4,
                    _ => bail!(
                        "Unsupported network of change address: {:?}",
                        change_address
                    ),
                };
                ensure!(prove_request.spell.outs.iter().all(|o| {
                    o.address.as_ref().is_some_and(|a| {
                        bitcoin::Address::from_str(a).is_ok_and(|a| a.is_valid_for_network(network))
                    })
                }));

                let charms_fee = get_charms_fee(&self.charms_fee_settings, total_cycles).to_sat();

                let total_sats_in: u64 = (&prove_request.spell.ins)
                    .iter()
                    .map(|i| {
                        let utxo_id = i.utxo_id.as_ref().expect("utxo_id is expected to be Some");
                        prev_txs_by_id
                            .get(&utxo_id.0)
                            .and_then(|prev_tx| {
                                if let Tx::Bitcoin(BitcoinTx(prev_tx)) = prev_tx {
                                    prev_tx
                                        .output
                                        .get(utxo_id.1 as usize)
                                        .map(|o| o.value.to_sat())
                                } else {
                                    None
                                }
                            })
                            .ok_or(anyhow!("utxo not found in prev_txs: {}", utxo_id))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?
                    .iter()
                    .sum();
                let total_sats_out: u64 = (&prove_request.spell.outs)
                    .iter()
                    .map(|o| o.amount.unwrap_or_default())
                    .sum();

                let funding_utxo_sats = prove_request.funding_utxo_value;

                ensure!(
                    total_sats_in + funding_utxo_sats > total_sats_out + charms_fee,
                    "total input value must be greater than total output value plus charms fee"
                );

                tracing::info!(total_sats_in, funding_utxo_sats, total_sats_out, charms_fee);
            }
            // CARDANO => {
            //     todo!()
            // }
            _ => bail!("unsupported chain: {}", prove_request.chain.as_str()),
        }
        Ok(total_cycles)
    }
}

pub fn from_hex_txs(prev_txs: &[String]) -> anyhow::Result<Vec<Tx>> {
    prev_txs.iter().map(|tx_hex| Tx::from_hex(tx_hex)).collect()
}

pub fn to_hex_txs(txs: &[Tx]) -> Vec<String> {
    txs.iter().map(|tx| tx.hex()).collect()
}

pub fn get_charms_fee(charms_fee: &Option<CharmsFee>, total_cycles: u64) -> Amount {
    charms_fee
        .as_ref()
        .map(|charms_fee| {
            Amount::from_sat(total_cycles * charms_fee.fee_rate / 1000000 + charms_fee.fee_base)
        })
        .unwrap_or_default()
}

pub fn align_spell_to_tx(
    norm_spell: NormalizedSpell,
    tx: &bitcoin::Transaction,
) -> anyhow::Result<NormalizedSpell> {
    let mut norm_spell = norm_spell;
    let spell_ins = norm_spell.tx.ins.as_ref().ok_or(anyhow!("no inputs"))?;

    ensure!(
        spell_ins.len() <= tx.input.len(),
        "spell inputs exceed transaction inputs"
    );
    ensure!(
        norm_spell.tx.outs.len() <= tx.output.len(),
        "spell outputs exceed transaction outputs"
    );

    for i in 0..spell_ins.len() {
        let utxo_id = &spell_ins[i];
        let out_point = tx.input[i].previous_output;
        ensure!(
            utxo_id.0 == TxId(out_point.txid.to_byte_array()),
            "input {} txid mismatch: {} != {}",
            i,
            utxo_id.0,
            out_point.txid
        );
        ensure!(
            utxo_id.1 == out_point.vout,
            "input {} vout mismatch: {} != {}",
            i,
            utxo_id.1,
            out_point.vout
        );
    }

    for i in spell_ins.len()..tx.input.len() {
        let out_point = tx.input[i].previous_output;
        let utxo_id = UtxoId(TxId(out_point.txid.to_byte_array()), out_point.vout);
        norm_spell.tx.ins.get_or_insert_with(Vec::new).push(utxo_id);
    }

    Ok(norm_spell)
}
