// Copyright (c) Mysten Labs, Inc.
// Modifications Copyright (c) 2024 IOTA Stiftung
// Modifications Copyright (c) 2026 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

//! Local proof model and verifier used by the lite RPC POC.

use anyhow::{anyhow, bail};
use iota_types::{
    base_types::ObjectRef,
    committee::Committee,
    effects::{
        TransactionEffects, TransactionEffectsAPI, TransactionEffectsExt, TransactionEvents,
    },
    event::{Event, EventID},
    messages_checkpoint::{CertifiedCheckpointSummary, CheckpointContents, EndOfEpochData},
    object::Object,
    transaction::Transaction,
};
use serde::{Deserialize, Serialize};

/// A proof for specific targets. It certifies a checkpoint summary and
/// optionally includes transaction evidence to certify objects and events.
#[derive(Debug, Serialize, Deserialize)]
pub struct Proof {
    /// Targets of the proof are a committee, objects, or events that need to be
    /// certified.
    pub targets: ProofTargets,

    /// A summary of the checkpoint being certified.
    pub checkpoint_summary: CertifiedCheckpointSummary,

    /// Optional transaction proof to certify objects and events.
    pub contents_proof: Option<TransactionProof>,
}

/// Aspects of IOTA state that need to be certified in a proof.
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct ProofTargets {
    /// Objects that need to be certified.
    pub objects: Vec<(ObjectRef, Object)>,

    /// Events that need to be certified.
    pub events: Vec<(EventID, Event)>,

    /// The next committee being certified.
    pub committee: Option<Committee>,
}

impl ProofTargets {
    /// Creates an empty proof target.
    ///
    /// An empty proof target still proves that the checkpoint summary is
    /// certified by the supplied committee.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an object target by object reference and content.
    pub fn add_object(mut self, object_ref: ObjectRef, object: Object) -> Self {
        self.objects.push((object_ref, object));
        self
    }

    /// Adds an event target by event ID and content.
    pub fn add_event(mut self, event_id: EventID, event: Event) -> Self {
        self.events.push((event_id, event));
        self
    }

    /// Adds the next committee as a proof target.
    pub fn set_committee(mut self, committee: Committee) -> Self {
        self.committee = Some(committee);
        self
    }
}

/// Transaction witness material used to certify objects and events.
#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionProof {
    /// Checkpoint contents including this transaction.
    pub checkpoint_contents: CheckpointContents,

    /// The transaction being certified.
    pub transaction: Transaction,

    /// The effects of the transaction being certified.
    pub effects: TransactionEffects,

    /// The events of the transaction being certified.
    pub events: Option<TransactionEvents>,
}

/// Verifies a proof against an authoritative committee.
///
/// A valid proof certifies the checkpoint summary and, when present, checks the
/// transaction witness material needed to authenticate target objects and events.
pub fn verify_proof(committee: &Committee, proof: &Proof) -> anyhow::Result<()> {
    let summary = &proof.checkpoint_summary;
    let contents = proof
        .contents_proof
        .as_ref()
        .map(|transaction_proof| &transaction_proof.checkpoint_contents);

    summary.verify_with_contents(committee, contents)?;
    verify_committee_target(summary, &proof.targets)?;
    verify_required_transaction_proof(proof)?;

    if let Some(transaction_proof) = &proof.contents_proof {
        verify_transaction_proof(summary, transaction_proof)?;
        verify_event_targets(&proof.targets, transaction_proof)?;
        verify_object_targets(&proof.targets, transaction_proof)?;
    }

    Ok(())
}

fn verify_committee_target(
    summary: &CertifiedCheckpointSummary,
    targets: &ProofTargets,
) -> anyhow::Result<()> {
    let Some(expected_committee) = &targets.committee else {
        return Ok(());
    };

    let Some(EndOfEpochData {
        next_epoch_committee,
        ..
    }) = &summary.end_of_epoch_data
    else {
        bail!("no end-of-epoch committee in checkpoint summary");
    };

    let actual_committee = Committee::new(
        summary
            .epoch()
            .checked_add(1)
            .ok_or_else(|| anyhow!("next epoch overflows u64"))?,
        next_epoch_committee.iter().cloned().collect(),
    );

    if actual_committee != *expected_committee {
        bail!("given committee does not match the end-of-epoch committee");
    }

    Ok(())
}

fn verify_required_transaction_proof(proof: &Proof) -> anyhow::Result<()> {
    if (!proof.targets.objects.is_empty() || !proof.targets.events.is_empty())
        && proof.contents_proof.is_none()
    {
        bail!("contents proof is missing");
    }

    Ok(())
}

fn verify_transaction_proof(
    summary: &CertifiedCheckpointSummary,
    transaction_proof: &TransactionProof,
) -> anyhow::Result<()> {
    let execution_digests = transaction_proof.effects.execution_digests();
    if transaction_proof.transaction.digest() != &execution_digests.transaction {
        bail!("transaction digest does not match the execution digest");
    }

    let transaction_is_in_checkpoint = transaction_proof
        .checkpoint_contents
        .enumerate_transactions(summary)
        .any(|(_, digests)| digests == &execution_digests);

    if !transaction_is_in_checkpoint {
        bail!("transaction digest not found in the checkpoint contents");
    }

    if transaction_proof.effects.events_digest()
        != transaction_proof
            .events
            .as_ref()
            .map(|events| events.digest())
            .as_ref()
    {
        bail!("events digest does not match the execution digest");
    }

    Ok(())
}

fn verify_event_targets(
    targets: &ProofTargets,
    transaction_proof: &TransactionProof,
) -> anyhow::Result<()> {
    if targets.events.is_empty() {
        return Ok(());
    }

    let Some(events) = &transaction_proof.events else {
        bail!("events digest is missing");
    };

    let execution_digests = transaction_proof.effects.execution_digests();
    for (event_id, event) in &targets.events {
        if event_id.tx_digest != execution_digests.transaction {
            bail!("event does not belong to the transaction");
        }

        let event_index = event_id.event_seq as usize;
        let Some(actual_event) = events.get(event_index) else {
            bail!("event sequence number out of bounds");
        };

        if actual_event != event {
            bail!("event contents do not match");
        }
    }

    Ok(())
}

fn verify_object_targets(
    targets: &ProofTargets,
    transaction_proof: &TransactionProof,
) -> anyhow::Result<()> {
    if targets.objects.is_empty() {
        return Ok(());
    }

    let changed_objects = transaction_proof.effects.all_changed_objects();
    for (object_ref, object) in &targets.objects {
        if object_ref != &object.compute_object_reference() {
            bail!("object reference does not match the object");
        }

        changed_objects
            .iter()
            .find(|changed_object_ref| &changed_object_ref.0 == object_ref)
            .ok_or_else(|| anyhow!("object not found"))?;
    }

    Ok(())
}
