# IOTA Lite RPC POC

This crate is a small proof of concept for building light-client proof material
from the gRPC API. It intentionally carries its own proof model and verifier so
the new shape can be tested without depending on the existing light-client CLI
and archive/checkpoint store flow.

The goal is to explore a thinner RPC-backed path:

1. Fetch proof inputs from a fullnode over gRPC.
2. Package those inputs into this crate's local proof type.
3. Verify the proof locally using a committee.

The POC focuses on transaction proofs first. Object and event targets can build
on the same split once the data-source boundary is clear.

## What Lite RPC Fetches

For a transaction digest, the POC fetches:

- transaction BCS and user signatures via `get_transactions`
- effects BCS via `get_transactions`
- event digest and event BCS when the effects reference events
- checkpoint sequence number via `get_transactions`
- checkpoint summary, signature, and contents via `get_checkpoint_by_sequence_number`

That gives the verifier a portable evidence bundle:

```text
certified checkpoint summary
checkpoint contents
transaction
effects
events, when present
```

The proof is then verified with this crate's local `verify_proof`.

## Client Shape

The public facade is `LiteRpcClient`. It wraps the SDK gRPC client and exposes
methods named after the trust model they use:

- `build_transaction_proof(digest)`
- `committee_from_trusted_node(epoch)`
- `committee_from_trusted_anchor(trusted_committee, target_epoch)`

## Committee Trust Modes

There are two different ways to get the committee used for verification. They
look similar in code, but they mean different things.

### Direct Committee Fetch

`committee_from_trusted_node(epoch)` calls:

```text
get_epoch(epoch, COMMITTEE)
```

This means the connected node is trusted to tell us the correct committee. It is
the right mode for:

- localnet smoke tests
- a trusted fullnode integration
- proving that the gRPC proof plumbing works

It is not independent light-client verification, because the same node provides
both the proof data and the committee used to verify that proof.

### Committee Walk From An Anchor

`committee_from_trusted_anchor(trusted_committee, target_epoch)` starts from a
committee the caller already trusts, then walks epoch by epoch. The CLI derives
that starting committee from a genesis blob:

```text
trusted committee for epoch N
get_epoch(N, LAST_CHECKPOINT)
get_checkpoint_by_sequence_number(last_checkpoint, CHECKPOINT_SUMMARY + CHECKPOINT_SIGNATURE)
verify the end-of-epoch checkpoint summary with committee N
extract next_epoch_committee
repeat until target epoch
```

This is the independent light-client model. The fullnode still serves the data,
but each transition is authenticated by the previously trusted committee.

For the command-line flow, the starting anchor is the network genesis blob.

## Why Both Modes Exist

Direct fetch is ergonomic and useful when the node is trusted. Committee walking
is slower and more involved, but it preserves the light-client trust model.

The distinction is:

```text
direct fetch = trusted-node verification
committee walk = anchored light-client verification
```

Both are useful for this POC. The localnet ignored test uses direct fetch because
the localnet node is trusted by the developer. The test-cluster test uses the
anchored walk so the transition logic stays exercised.

## CLI

The crate includes a small binary:

```sh
cargo run --bin iota-lite-poc -- --help
```

By default, commands connect to `http://127.0.0.1:50051`. You can override that
with `--grpc-url` or `IOTA_LITE_POC_GRPC_URL`.

Create a transaction proof:

```sh
cargo run --bin iota-lite-poc -- create-transaction-proof \
  --transaction-digest <transaction-digest> \
  --output proof.json
```

Verify a proof by trusting the connected node for committee data:

```sh
cargo run --bin iota-lite-poc -- verify-proof \
  --proof proof.json
```

Fetch a committee from a trusted node:

```sh
cargo run --bin iota-lite-poc -- fetch-committee \
  --epoch 0 \
  --output epoch-0-committee.json
```

Walk committee lineage from a trusted genesis blob:

```sh
cargo run --bin iota-lite-poc -- walk-committee \
  --genesis-blob genesis.blob \
  --target-epoch 1 \
  --output epoch-1-committee.json
```

Verify a proof with a committee file instead of trusting the node directly:

```sh
cargo run --bin iota-lite-poc -- verify-proof \
  --proof proof.json \
  --committee epoch-1-committee.json
```

## Running The Tests

Run the deterministic test-cluster path:

```sh
cargo test --test grpc_transaction_proof \
  builds_and_verifies_transaction_proof_from_grpc
```

Run the ignored localnet path after starting localnet with gRPC enabled:

```sh
IOTA_LITE_POC_GRPC_URL=http://127.0.0.1:50051 \
cargo test --test grpc_transaction_proof \
  builds_and_verifies_transaction_proof_from_localnet_grpc -- --ignored
```

If the latest checkpoint has no transaction, provide one explicitly:

```sh
IOTA_LITE_POC_GRPC_URL=http://127.0.0.1:50051 \
IOTA_LITE_POC_TX_DIGEST=<transaction-digest> \
cargo test --test grpc_transaction_proof \
  builds_and_verifies_transaction_proof_from_localnet_grpc -- --ignored
```

## Current Limits

- The POC is transaction-proof only.
- The localnet test assumes the gRPC node is trusted for committee data.
- gRPC can only serve data the node still has available. For pruned history,
  archive or another historical source may still be needed.
- This crate is experimental and deliberately self-contained: it carries the
  minimal proof model and verifier locally instead of depending on
  `iota-light-client`.
