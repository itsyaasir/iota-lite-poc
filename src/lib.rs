// Copyright (c) 2026 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

//! Experimental gRPC-backed light-client proof construction.
//!
//! The crate deliberately keeps proof construction and committee trust
//! explicit: callers either trust the connected node for committee data, or
//! they provide a trusted committee anchor and authenticate every epoch
//! transition from there.

use anyhow::{anyhow, bail, Context};
use iota_grpc_client::{
    read_mask_fields::{CheckpointResponseField, EpochField, ServiceInfoField, TransactionField},
    Client as GrpcClient, ReadMask,
};
use iota_sdk_types::{Digest, SignedTransaction};
use iota_types::{
    committee::{Committee, EpochId},
    effects::{TransactionEffects, TransactionEffectsAPI, TransactionEvents},
    messages_checkpoint::{CertifiedCheckpointSummary, CheckpointContents, EndOfEpochData},
    transaction::Transaction,
};

mod proof;

pub use proof::{verify_proof, Proof, ProofTargets, TransactionProof};

type CheckpointResponse = iota_grpc_client::CheckpointResponse;
type ExecutedTransaction = iota_grpc_types::v1::transaction::ExecutedTransaction;

/// Thin gRPC facade for building light-client proof material.
///
/// `LiteRpcClient` does not run a local checkpoint/archive store. It fetches
/// the witness data needed for a specific proof from a gRPC node, then packages
/// that data into this crate's local proof format.
#[derive(Clone)]
pub struct LiteRpcClient {
    grpc_client: GrpcClient,
}

impl LiteRpcClient {
    /// Creates a lite RPC client from an existing SDK gRPC client.
    pub fn new(grpc_client: GrpcClient) -> Self {
        Self { grpc_client }
    }

    /// Returns the underlying SDK gRPC client.
    pub fn grpc_client(&self) -> &GrpcClient {
        &self.grpc_client
    }

    /// Fetches the committee for `epoch` directly from the connected node.
    ///
    /// Use this only when the node is trusted for committee data, for example
    /// in localnet smoke tests or trusted infrastructure integration tests.
    /// This does not authenticate committee lineage.
    pub async fn committee_from_trusted_node(&self, epoch: EpochId) -> anyhow::Result<Committee> {
        self.fetch_epoch_committee(epoch).await
    }

    /// Authenticates committee transitions from `trusted_committee` to
    /// `target_epoch`.
    ///
    /// This is the light-client trust path: the node serves epoch/checkpoint
    /// data, but each transition is verified by the currently trusted
    /// committee.
    pub async fn committee_from_trusted_anchor(
        &self,
        trusted_committee: Committee,
        target_epoch: EpochId,
    ) -> anyhow::Result<Committee> {
        ensure_target_epoch_is_reachable(
            trusted_committee.epoch,
            target_epoch,
            self.current_epoch().await?,
        )?;

        let mut verified_committee = trusted_committee;
        while verified_committee.epoch < target_epoch {
            verified_committee = self.next_verified_committee(&verified_committee).await?;
        }

        Ok(verified_committee)
    }

    /// Builds a portable transaction proof from gRPC witness data.
    pub async fn build_transaction_proof(
        &self,
        transaction_digest: Digest,
    ) -> anyhow::Result<Proof> {
        let executed_transaction = self.fetch_executed_transaction(transaction_digest).await?;
        let checkpoint_sequence_number = executed_transaction
            .checkpoint_sequence_number()
            .with_context(|| {
                format!("transaction {transaction_digest} response is missing checkpoint sequence")
            })?;

        let checkpoint = self
            .fetch_checkpoint_with_contents(checkpoint_sequence_number)
            .await?;

        let effects = effects_from_response(&executed_transaction)?;
        let events = events_from_response_if_present(&executed_transaction, &effects)?;

        Ok(Proof {
            targets: ProofTargets::new(),
            checkpoint_summary: checkpoint_summary_from_response(&checkpoint)?,
            contents_proof: Some(TransactionProof {
                checkpoint_contents: checkpoint_contents_from_response(&checkpoint)?,
                transaction: transaction_from_response(&executed_transaction)?,
                effects,
                events,
            }),
        })
    }

    async fn fetch_epoch_committee(&self, epoch: EpochId) -> anyhow::Result<Committee> {
        let epoch_response = self
            .grpc_client
            .get_epoch(Some(epoch), Some(ReadMask::from(EpochField::COMMITTEE)))
            .await
            .with_context(|| format!("failed to fetch epoch {epoch} committee over gRPC"))?
            .into_inner();

        epoch_response
            .committee()
            .with_context(|| format!("epoch {epoch} response is missing committee"))?
            .try_into()
            .map_err(|err| anyhow!("failed to convert epoch {epoch} committee: {err}"))
    }

    async fn fetch_executed_transaction(
        &self,
        transaction_digest: Digest,
    ) -> anyhow::Result<ExecutedTransaction> {
        let transactions = self
            .grpc_client
            .get_transactions(
                &[transaction_digest],
                Some(ReadMask::from(TRANSACTION_PROOF_FIELDS)),
            )
            .await
            .with_context(|| {
                format!("failed to fetch transaction {transaction_digest} over gRPC")
            })?;

        transactions
            .body()
            .first()
            .cloned()
            .with_context(|| format!("gRPC returned no transaction for {transaction_digest}"))
    }

    async fn fetch_checkpoint_with_contents(
        &self,
        sequence_number: u64,
    ) -> anyhow::Result<CheckpointResponse> {
        self.fetch_checkpoint(sequence_number, CHECKPOINT_PROOF_FIELDS)
            .await
    }

    async fn fetch_certified_checkpoint_summary(
        &self,
        sequence_number: u64,
    ) -> anyhow::Result<CertifiedCheckpointSummary> {
        let checkpoint = self
            .fetch_checkpoint(sequence_number, CHECKPOINT_SUMMARY_FIELDS)
            .await?;
        checkpoint_summary_from_response(&checkpoint)
    }

    async fn fetch_checkpoint(
        &self,
        sequence_number: u64,
        fields: &[&str],
    ) -> anyhow::Result<CheckpointResponse> {
        self.grpc_client
            .get_checkpoint_by_sequence_number(
                sequence_number,
                Some(ReadMask::from(fields)),
                None,
                None,
            )
            .await
            .with_context(|| format!("failed to fetch checkpoint {sequence_number} over gRPC"))
            .map(|response| response.into_inner())
    }

    async fn current_epoch(&self) -> anyhow::Result<EpochId> {
        self.grpc_client
            .get_service_info(Some(ReadMask::from(ServiceInfoField::EPOCH)))
            .await
            .context("failed to fetch service info over gRPC")?
            .body()
            .epoch
            .context("service info response is missing current epoch")
    }

    async fn epoch_last_checkpoint(&self, epoch: EpochId) -> anyhow::Result<u64> {
        let epoch_response = self
            .grpc_client
            .get_epoch(
                Some(epoch),
                Some(ReadMask::from(EpochField::LAST_CHECKPOINT)),
            )
            .await
            .with_context(|| format!("failed to fetch epoch {epoch} info over gRPC"))?
            .into_inner();

        epoch_response
            .last_checkpoint
            .with_context(|| format!("epoch {epoch} response is missing last checkpoint"))
    }

    async fn next_verified_committee(
        &self,
        current_committee: &Committee,
    ) -> anyhow::Result<Committee> {
        let last_checkpoint = self.epoch_last_checkpoint(current_committee.epoch).await?;
        let certified_summary = self
            .fetch_certified_checkpoint_summary(last_checkpoint)
            .await?;

        certified_summary
            .clone()
            .try_into_verified(current_committee)
            .with_context(|| {
                format!(
                    "failed to verify epoch {} transition checkpoint {last_checkpoint}",
                    current_committee.epoch
                )
            })?;

        next_committee_from_end_of_epoch_summary(&certified_summary, last_checkpoint)
    }
}

const TRANSACTION_PROOF_FIELDS: &[&str] = &[
    TransactionField::TRANSACTION_BCS,
    TransactionField::SIGNATURES,
    TransactionField::EFFECTS_BCS,
    TransactionField::EVENTS_DIGEST,
    TransactionField::EVENTS_EVENTS_BCS,
    TransactionField::CHECKPOINT,
];

const CHECKPOINT_PROOF_FIELDS: &[&str] = &[
    CheckpointResponseField::CHECKPOINT_SUMMARY_BCS,
    CheckpointResponseField::CHECKPOINT_SIGNATURE,
    CheckpointResponseField::CHECKPOINT_CONTENTS_BCS,
];

const CHECKPOINT_SUMMARY_FIELDS: &[&str] = &[
    CheckpointResponseField::CHECKPOINT_SUMMARY_BCS,
    CheckpointResponseField::CHECKPOINT_SIGNATURE,
];

fn ensure_target_epoch_is_reachable(
    trusted_epoch: EpochId,
    target_epoch: EpochId,
    node_current_epoch: EpochId,
) -> anyhow::Result<()> {
    if target_epoch < trusted_epoch {
        bail!("target epoch {target_epoch} is before trusted committee epoch {trusted_epoch}");
    }

    if target_epoch > node_current_epoch {
        bail!("target epoch {target_epoch} is ahead of node current epoch {node_current_epoch}");
    }

    Ok(())
}

fn checkpoint_summary_from_response(
    checkpoint: &CheckpointResponse,
) -> anyhow::Result<CertifiedCheckpointSummary> {
    checkpoint
        .signed_summary()
        .context("checkpoint response is missing signed summary")?
        .try_into()
        .map_err(|err| anyhow!("failed to convert checkpoint summary: {err}"))
}

fn checkpoint_contents_from_response(
    checkpoint: &CheckpointResponse,
) -> anyhow::Result<CheckpointContents> {
    checkpoint
        .contents()
        .context("checkpoint response is missing contents BCS")?
        .contents()
        .context("failed to deserialize checkpoint contents")?
        .try_into()
        .map_err(|err| anyhow!("failed to convert checkpoint contents: {err}"))
}

fn transaction_from_response(transaction: &ExecutedTransaction) -> anyhow::Result<Transaction> {
    let sdk_transaction = transaction
        .transaction()
        .context("transaction response is missing transaction BCS")?
        .transaction()
        .context("failed to deserialize transaction BCS")?;
    let signatures = transaction
        .signatures()
        .context("transaction response is missing signatures")?
        .signatures
        .iter()
        .map(|signature| signature.signature())
        .collect::<Result<Vec<_>, _>>()
        .context("failed to deserialize transaction signatures")?;

    SignedTransaction {
        transaction: sdk_transaction,
        signatures,
    }
    .try_into()
    .map_err(|err| anyhow!("failed to convert signed transaction: {err}"))
}

fn effects_from_response(transaction: &ExecutedTransaction) -> anyhow::Result<TransactionEffects> {
    transaction
        .effects()
        .context("transaction response is missing effects BCS")?
        .effects()
        .context("failed to deserialize transaction effects")
}

fn events_from_response_if_present(
    transaction: &ExecutedTransaction,
    effects: &TransactionEffects,
) -> anyhow::Result<Option<TransactionEvents>> {
    if effects.events_digest().is_none() {
        return Ok(None);
    }

    transaction
        .events()
        .context("transaction effects refer to events but event data is missing")?
        .events()
        .context("failed to deserialize transaction events")
        .map(Some)
}

fn next_committee_from_end_of_epoch_summary(
    summary: &CertifiedCheckpointSummary,
    checkpoint_sequence_number: u64,
) -> anyhow::Result<Committee> {
    let Some(EndOfEpochData {
        next_epoch_committee,
        ..
    }) = &summary.end_of_epoch_data
    else {
        bail!("checkpoint {checkpoint_sequence_number} is not an end-of-epoch checkpoint");
    };

    Ok(Committee::new(
        summary
            .epoch()
            .checked_add(1)
            .context("next epoch overflows u64")?,
        next_epoch_committee.iter().cloned().collect(),
    ))
}
