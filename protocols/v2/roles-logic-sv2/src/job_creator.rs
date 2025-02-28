use crate::utils::Id;
use binary_sv2::B064K;
use bitcoin::{
    blockdata::{
        script::Script,
        transaction::{OutPoint, Transaction, TxIn, TxOut},
    },
    util::psbt::serialize::Serialize,
};
pub use bitcoin::{
    secp256k1::SecretKey,
    util::ecdsa::{PrivateKey, PublicKey},
};
use mining_sv2::NewExtendedMiningJob;
use std::{collections::HashMap, convert::TryInto};
use template_distribution_sv2::{NewTemplate, SetNewPrevHash};

const SCRIPT_PREFIX_LEN: usize = 4;
const PREV_OUT_LEN: usize = 38;
const EXTRANONCE_LEN: usize = 32;

/// Used by pool one for each group channel
/// extended and standard channel not supported
#[derive(Debug)]
struct JobCreator {
    group_channel_id: u32,
    job_ids: Id,
    version_rolling_allowed: bool,
    template_id_to_job_id: HashMap<u64, u32>,
    // TODO this is always 32 (required by the spec) so put that in const_sv2 crate
    #[allow(dead_code)]
    extranonce_prefix_len: u8,
}

impl JobCreator {
    fn new_extended_job(
        &mut self,
        new_template: &mut NewTemplate,
        coinbase_outputs: &[TxOut],
    ) -> NewExtendedMiningJob<'static> {
        assert!(
            new_template.coinbase_tx_outputs_count == 0,
            "node provided outputs not supported yet"
        );
        let script_prefix = new_template.coinbase_prefix.to_vec();
        // TODO the below should return an error not panic
        assert!(
            script_prefix.len() > 3,
            "Bitocin blockchain should be at least 16 block long"
        );
        let bip34_len = script_prefix[1] as usize;
        let bip34_bytes = script_prefix[1..2 + bip34_len].to_vec();

        let coinbase = self.coinbase(
            bip34_bytes,
            new_template
                .coinbase_tx_version
                .try_into()
                .expect("invalid version"),
            new_template.coinbase_tx_locktime,
            new_template.coinbase_tx_input_sequence,
            coinbase_outputs,
        );
        let new_extended_mining_job: NewExtendedMiningJob<'static> = NewExtendedMiningJob {
            channel_id: self.group_channel_id,
            job_id: self.job_ids.next(),
            future_job: new_template.future_template,
            version: new_template.version,
            version_rolling_allowed: self.version_rolling_allowed,
            merkle_path: new_template.merkle_path.clone().into_static(),
            coinbase_tx_prefix: Self::coinbase_tx_prefix(&coinbase, SCRIPT_PREFIX_LEN),
            coinbase_tx_suffix: Self::coinbase_tx_suffix(&coinbase, SCRIPT_PREFIX_LEN),
        };
        self.template_id_to_job_id
            .insert(new_template.template_id, new_extended_mining_job.job_id);
        new_extended_mining_job
    }

    fn get_job_id(&self, template_id: u64) -> Option<u32> {
        self.template_id_to_job_id.get(&template_id).copied()
    }

    fn coinbase_tx_prefix(
        coinbase: &Transaction,
        coinbase_tx_input_script_prefix_byte_len: usize,
    ) -> B064K<'static> {
        let encoded = coinbase.serialize();
        // add 1 cause the script header (len of script) is 1 byte
        let r = encoded
            [0..SCRIPT_PREFIX_LEN + coinbase_tx_input_script_prefix_byte_len + PREV_OUT_LEN]
            .to_vec();
        r.try_into().unwrap()
    }

    fn coinbase_tx_suffix(
        coinbase: &Transaction,
        coinbase_tx_input_script_prefix_byte_len: usize,
    ) -> B064K<'static> {
        let encoded = coinbase.serialize();
        let r = encoded[SCRIPT_PREFIX_LEN
            + coinbase_tx_input_script_prefix_byte_len
            + PREV_OUT_LEN
            + EXTRANONCE_LEN..]
            .to_vec();
        r.try_into().unwrap()
    }
    //fn coinbase_script(&self, mut coinbase_tx_input_script_prefix: Vec<u8>) -> Script {
    //    let remaning_len = self.extranonce_prefix_len;
    //    coinbase_tx_input_script_prefix.append(&mut vec![0, remaning_len]);
    //    coinbase_tx_input_script_prefix
    //        .try_into()
    //        .expect("invalid script")
    //}

    /// coinbase_tx_input_script_prefix: extranonce prefix (script lenght + bip34 block height) provided by the node
    /// TODO it assume that NewTemplate.coinbase_tx_outputs == 0
    fn coinbase(
        &self,
        mut bip34_bytes: Vec<u8>,
        version: i32,
        lock_time: u32,
        sequence: u32,
        coinbase_outputs: &[TxOut],
    ) -> Transaction {
        bip34_bytes.extend_from_slice(&[0; EXTRANONCE_LEN]);
        let tx_in = TxIn {
            previous_output: OutPoint::null(),
            script_sig: bip34_bytes.try_into().unwrap(),
            sequence,
            witness: vec![],
        };
        Transaction {
            version,
            lock_time,
            input: vec![tx_in],
            output: coinbase_outputs.to_vec(),
        }
    }
}

/// Used by pool add a JobCreator for each group channel
/// extended and standard channel not supported
#[derive(Debug)]
pub struct JobsCreators {
    jobs_creators: Vec<JobCreator>,
    /// Computed by the pool
    coinbase_outputs: Vec<TxOut>,
    block_reward_staoshi: u64,
    pub_key: PublicKey,
    lasts_new_template: Vec<NewTemplate<'static>>,
    //last_prev_hash: Pr
}

impl JobsCreators {
    pub fn new(block_reward_staoshi: u64, pub_key: PublicKey) -> Self {
        Self {
            jobs_creators: vec![],
            coinbase_outputs: Self::new_outputs(block_reward_staoshi, pub_key),
            block_reward_staoshi,
            pub_key,
            lasts_new_template: Vec::new(),
        }
    }

    pub fn new_outputs(block_reward_staoshi: u64, pub_key: PublicKey) -> Vec<TxOut> {
        let script_pubkey = Script::new_v0_wpkh(&pub_key.wpubkey_hash().unwrap());
        vec![TxOut {
            value: block_reward_staoshi,
            script_pubkey,
        }]
    }

    pub fn on_new_template(
        &mut self,
        template: &mut NewTemplate,
    ) -> HashMap<u32, NewExtendedMiningJob<'static>> {
        if template.coinbase_tx_value_remaining != self.block_reward_staoshi {
            self.block_reward_staoshi = template.coinbase_tx_value_remaining;
            self.coinbase_outputs =
                Self::new_outputs(template.coinbase_tx_value_remaining, self.pub_key);
        }

        let mut new_extended_jobs = HashMap::new();
        for creator in &mut self.jobs_creators {
            let job = creator.new_extended_job(template, &self.coinbase_outputs);
            new_extended_jobs.insert(job.channel_id, job);
        }
        self.lasts_new_template.push(template.as_static());

        new_extended_jobs
    }

    fn reset_new_templates(&mut self, template: Option<NewTemplate<'static>>) {
        match template {
            Some(t) => self.lasts_new_template = vec![t],
            None => self.lasts_new_template = vec![],
        }
    }

    pub fn on_new_prev_hash(&mut self, prev_hash: &SetNewPrevHash<'static>) {
        let template: Vec<NewTemplate<'static>> = self
            .lasts_new_template
            .clone()
            .into_iter()
            .filter(|a| a.template_id == prev_hash.template_id)
            .collect();
        match template.len() {
            0 => self.reset_new_templates(None),
            1 => self.reset_new_templates(Some(template[0].clone())),
            _ => todo!("{:#?}", template.len()),
        }
    }

    pub fn new_group_channel(
        &mut self,
        group_channel_id: u32,
        version_rolling_allowed: bool,
        extranonce_prefix_len: u8,
        // Vec<(job,template_id)>
    ) -> Vec<(NewExtendedMiningJob<'static>, u64)> {
        let mut jc = JobCreator {
            group_channel_id,
            job_ids: Id::new(),
            version_rolling_allowed,
            template_id_to_job_id: HashMap::new(),
            extranonce_prefix_len,
        };
        let mut res = Vec::new();
        for mut template in self.lasts_new_template.clone() {
            res.push((
                jc.new_extended_job(&mut template, &self.coinbase_outputs),
                template.template_id,
            ));
        }
        self.jobs_creators.push(jc);
        res
    }

    pub fn job_id_from_template(&self, template_id: u64, group_id: u32) -> Option<u32> {
        for jc in &self.jobs_creators {
            if jc.group_channel_id == group_id {
                return jc.get_job_id(template_id);
            }
        }
        None
    }
}
