// Copyright (c) 2026 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use iota_grpc_client::Client as GrpcClient;
use iota_lite_poc::{verify_proof, LiteRpcClient, Proof};
use iota_sdk_types::Digest;
use iota_types::committee::{Committee, EpochId};

const DEFAULT_GRPC_URL: &str = "http://127.0.0.1:50051";

#[derive(Parser)]
#[command(
    name = "iota-lite-poc",
    about = "Build and verify IOTA lite RPC proof files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a transaction proof from gRPC witness data.
    CreateTransactionProof {
        /// gRPC endpoint to fetch proof inputs from.
        #[arg(long, env = "IOTA_LITE_POC_GRPC_URL", default_value = DEFAULT_GRPC_URL)]
        grpc_url: String,

        /// Transaction digest to prove.
        #[arg(long)]
        transaction_digest: String,

        /// Proof JSON file to write.
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Verify a proof file.
    VerifyProof {
        /// gRPC endpoint used when no committee file is supplied.
        #[arg(long, env = "IOTA_LITE_POC_GRPC_URL", default_value = DEFAULT_GRPC_URL)]
        grpc_url: String,

        /// Proof JSON file to verify.
        #[arg(short, long)]
        proof: PathBuf,

        /// Trusted committee JSON file. If omitted, the committee is fetched
        /// directly from the gRPC node, so the node is trusted for committee data.
        #[arg(long)]
        committee: Option<PathBuf>,
    },

    /// Fetch a committee directly from a trusted gRPC node.
    FetchCommittee {
        /// gRPC endpoint to fetch committee data from.
        #[arg(long, env = "IOTA_LITE_POC_GRPC_URL", default_value = DEFAULT_GRPC_URL)]
        grpc_url: String,

        /// Epoch whose committee should be fetched.
        #[arg(long)]
        epoch: EpochId,

        /// Committee JSON file to write.
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Authenticate committee transitions from a trusted committee anchor.
    WalkCommittee {
        /// gRPC endpoint to fetch epoch/checkpoint transition data from.
        #[arg(long, env = "IOTA_LITE_POC_GRPC_URL", default_value = DEFAULT_GRPC_URL)]
        grpc_url: String,

        /// Trusted starting committee JSON file.
        #[arg(long)]
        trusted_committee: PathBuf,

        /// Target epoch to derive from the trusted committee.
        #[arg(long)]
        target_epoch: EpochId,

        /// Verified committee JSON file to write.
        #[arg(short, long)]
        output: PathBuf,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::CreateTransactionProof {
            grpc_url,
            transaction_digest,
            output,
        } => create_transaction_proof(&grpc_url, &transaction_digest, &output).await,
        Command::VerifyProof {
            grpc_url,
            proof,
            committee,
        } => verify_proof_file(&grpc_url, &proof, committee.as_deref()).await,
        Command::FetchCommittee {
            grpc_url,
            epoch,
            output,
        } => fetch_committee(&grpc_url, epoch, &output).await,
        Command::WalkCommittee {
            grpc_url,
            trusted_committee,
            target_epoch,
            output,
        } => walk_committee(&grpc_url, &trusted_committee, target_epoch, &output).await,
    }
}

async fn create_transaction_proof(
    grpc_url: &str,
    transaction_digest: &str,
    output: &Path,
) -> anyhow::Result<()> {
    let transaction_digest = Digest::from_str(transaction_digest)
        .with_context(|| format!("failed to parse transaction digest {transaction_digest}"))?;
    let lite_rpc_client = lite_rpc_client(grpc_url)?;
    let proof = lite_rpc_client
        .build_transaction_proof(transaction_digest)
        .await
        .with_context(|| format!("failed to build transaction proof for {transaction_digest}"))?;

    write_json(output, &proof)
        .with_context(|| format!("failed to write transaction proof to {}", output.display()))?;

    println!(
        "created proof {}\ncheckpoint: {}\ncheckpoint sequence number: {}\nepoch: {}",
        output.display(),
        proof.checkpoint_summary.digest(),
        proof.checkpoint_summary.sequence_number,
        proof.checkpoint_summary.epoch,
    );
    Ok(())
}

async fn verify_proof_file(
    grpc_url: &str,
    proof_path: &Path,
    committee_path: Option<&Path>,
) -> anyhow::Result<()> {
    let proof: Proof = read_json(proof_path)
        .with_context(|| format!("failed to read proof from {}", proof_path.display()))?;
    let committee = match committee_path {
        Some(path) => read_json(path)
            .with_context(|| format!("failed to read committee from {}", path.display()))?,
        None => {
            let lite_rpc_client = lite_rpc_client(grpc_url)?;
            lite_rpc_client
                .committee_from_trusted_node(proof.checkpoint_summary.epoch())
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch epoch {} committee from trusted node",
                        proof.checkpoint_summary.epoch()
                    )
                })?
        }
    };

    verify_proof(&committee, &proof)
        .with_context(|| format!("failed to verify proof {}", proof_path.display()))?;

    println!(
        "verified proof {}\ncheckpoint: {}\ncheckpoint sequence number: {}\nepoch: {}\ncommittee epoch: {}",
        proof_path.display(),
        proof.checkpoint_summary.digest(),
        proof.checkpoint_summary.sequence_number,
        proof.checkpoint_summary.epoch,
        committee.epoch,
    );
    Ok(())
}

async fn fetch_committee(grpc_url: &str, epoch: EpochId, output: &Path) -> anyhow::Result<()> {
    let lite_rpc_client = lite_rpc_client(grpc_url)?;
    let committee = lite_rpc_client
        .committee_from_trusted_node(epoch)
        .await
        .with_context(|| format!("failed to fetch committee for epoch {epoch}"))?;

    write_json(output, &committee)
        .with_context(|| format!("failed to write committee to {}", output.display()))?;

    println!(
        "fetched committee {}\nepoch: {}\nvoting rights: {}",
        output.display(),
        committee.epoch,
        committee.total_votes(),
    );
    Ok(())
}

async fn walk_committee(
    grpc_url: &str,
    trusted_committee_path: &Path,
    target_epoch: EpochId,
    output: &Path,
) -> anyhow::Result<()> {
    let trusted_committee: Committee = read_json(trusted_committee_path).with_context(|| {
        format!(
            "failed to read trusted committee from {}",
            trusted_committee_path.display()
        )
    })?;
    let trusted_epoch = trusted_committee.epoch;
    let lite_rpc_client = lite_rpc_client(grpc_url)?;
    let committee = lite_rpc_client
        .committee_from_trusted_anchor(trusted_committee, target_epoch)
        .await
        .with_context(|| {
            format!("failed to walk committee from epoch {trusted_epoch} to {target_epoch}")
        })?;

    write_json(output, &committee)
        .with_context(|| format!("failed to write committee to {}", output.display()))?;

    println!(
        "verified committee {}\ntrusted epoch: {}\ntarget epoch: {}\nvoting rights: {}",
        output.display(),
        trusted_epoch,
        committee.epoch,
        committee.total_votes(),
    );
    Ok(())
}

fn lite_rpc_client(grpc_url: &str) -> anyhow::Result<LiteRpcClient> {
    GrpcClient::new(grpc_url)
        .map(LiteRpcClient::new)
        .with_context(|| format!("failed to connect to gRPC endpoint {grpc_url}"))
}

fn read_json<T>(path: &Path) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let file = File::open(path)?;
    serde_json::from_reader(BufReader::new(file)).map_err(Into::into)
}

fn write_json<T>(path: &Path, value: &T) -> anyhow::Result<()>
where
    T: serde::Serialize,
{
    let file = File::create(path)?;
    serde_json::to_writer_pretty(BufWriter::new(file), value)?;
    Ok(())
}
