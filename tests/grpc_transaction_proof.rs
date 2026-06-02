// Copyright (c) 2026 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use std::{env, str::FromStr};

use iota_grpc_client::{
    Client, ReadMask,
    read_mask_fields::{CheckpointResponseField, CheckpointTransactionField},
};
use iota_light_client::proof::verify_proof;
use iota_lite_poc::LiteRpcClient;
use iota_macros::sim_test;
use iota_sdk_types::Digest;
use iota_test_transaction_builder::make_transfer_iota_transaction;
use test_cluster::TestClusterBuilder;

#[sim_test]
async fn builds_and_verifies_transaction_proof_from_grpc() {
    let test_cluster = TestClusterBuilder::new()
        .with_fullnode_enable_grpc_api(true)
        .disable_fullnode_pruning()
        .with_num_validators(1)
        .build()
        .await;

    test_cluster.wait_for_checkpoint(1, None).await;

    let client = Client::new(test_cluster.grpc_url()).expect("connect to gRPC service");
    let genesis_committee = test_cluster
        .get_genesis()
        .committee()
        .expect("load genesis committee");
    test_cluster.force_new_epoch().await;

    let baseline_checkpoint = client
        .get_checkpoint_latest(
            Some(ReadMask::from(
                CheckpointResponseField::CHECKPOINT_SEQUENCE_NUMBER,
            )),
            None,
            None,
        )
        .await
        .expect("fetch latest checkpoint")
        .body()
        .sequence_number();

    let tx = make_transfer_iota_transaction(&test_cluster.wallet, None, None).await;
    let digest = Digest::new(tx.digest().into_inner());

    test_cluster
        .wallet
        .execute_transaction_may_fail(tx)
        .await
        .expect("execute transfer transaction");
    test_cluster
        .wait_for_checkpoint(baseline_checkpoint + 2, None)
        .await;

    let lite_rpc_client = LiteRpcClient::new(client);
    let proof = lite_rpc_client
        .build_transaction_proof(digest)
        .await
        .expect("construct transaction proof from gRPC");
    let proof_epoch = proof.checkpoint_summary.epoch();
    assert_eq!(proof_epoch, 1, "proof should be for the post-genesis epoch");

    let committee = lite_rpc_client
        .committee_from_trusted_anchor(genesis_committee, proof_epoch)
        .await
        .expect("derive proof epoch committee from genesis over gRPC");

    verify_proof(&committee, &proof).expect("verify gRPC transaction proof");
}

#[sim_test]
async fn fetches_committee_directly_from_trusted_node() {
    let test_cluster = TestClusterBuilder::new()
        .with_fullnode_enable_grpc_api(true)
        .disable_fullnode_pruning()
        .with_num_validators(1)
        .build()
        .await;

    test_cluster.wait_for_checkpoint(1, None).await;

    let client = Client::new(test_cluster.grpc_url()).expect("connect to gRPC service");
    let lite_rpc_client = LiteRpcClient::new(client);

    let committee = lite_rpc_client
        .committee_from_trusted_node(0)
        .await
        .expect("fetch committee directly from trusted node");

    assert_eq!(committee.epoch, 0);
}

#[tokio::test]
#[ignore = "requires a running trusted localnet gRPC endpoint"]
async fn builds_and_verifies_transaction_proof_from_localnet_grpc() {
    let grpc_url =
        env::var("IOTA_LITE_POC_GRPC_URL").unwrap_or_else(|_| "http://127.0.0.1:50051".to_string());

    let client = Client::new(grpc_url).expect("connect to localnet gRPC service");

    let digest = match env::var("IOTA_LITE_POC_TX_DIGEST") {
        Ok(digest) => Digest::from_str(&digest).expect("parse IOTA_LITE_POC_TX_DIGEST"),
        Err(_) => latest_checkpoint_transaction_digest(&client).await,
    };

    let lite_rpc_client = LiteRpcClient::new(client);
    let proof = lite_rpc_client
        .build_transaction_proof(digest)
        .await
        .expect("construct transaction proof from localnet gRPC");

    let committee = lite_rpc_client
        .committee_from_trusted_node(proof.checkpoint_summary.epoch())
        .await
        .expect("fetch proof epoch committee from trusted localnet node");

    verify_proof(&committee, &proof).expect("verify localnet gRPC transaction proof");
}

async fn latest_checkpoint_transaction_digest(client: &Client) -> Digest {
    let checkpoint = client
        .get_checkpoint_latest(
            Some(ReadMask::from(&[
                CheckpointTransactionField::TRANSACTION_DIGEST,
            ])),
            None,
            None,
        )
        .await
        .expect("fetch latest localnet checkpoint");

    checkpoint
        .body()
        .executed_transactions()
        .iter()
        .find_map(|transaction| transaction.transaction().ok()?.digest().ok())
        .expect("latest localnet checkpoint should contain a transaction; set IOTA_LITE_POC_TX_DIGEST if it does not")
}
