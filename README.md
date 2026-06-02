# IOTA Lite RPC POC

This crate is a small proof of concept for building light-client proof material
from the gRPC API. It intentionally lives outside `iota-light-client` so the new
shape can be tested without disturbing the existing CLI and archive/checkpoint
store flow.

The goal is to explore a thinner RPC-backed path:

1. Fetch proof inputs from a fullnode over gRPC.
2. Package those inputs into the existing `iota-light-client` proof type.
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

The proof is then verified with the existing `iota_light_client::proof::verify_proof`.

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
committee the caller already trusts, then walks epoch by epoch:

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

The starting anchor can come from:

- a localnet genesis committee
- an official genesis blob
- a pinned trusted committee
- another trusted checkpoint/committee bundle

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

## Running The Tests

Run the deterministic test-cluster path:

```sh
cargo test -p iota-lite-poc --test grpc_transaction_proof \
  builds_and_verifies_transaction_proof_from_grpc
```

Run the ignored localnet path after starting localnet with gRPC enabled:

```sh
IOTA_LITE_POC_GRPC_URL=http://127.0.0.1:50051 \
cargo test -p iota-lite-poc --test grpc_transaction_proof \
  builds_and_verifies_transaction_proof_from_localnet_grpc -- --ignored
```

If the latest checkpoint has no transaction, provide one explicitly:

```sh
IOTA_LITE_POC_GRPC_URL=http://127.0.0.1:50051 \
IOTA_LITE_POC_TX_DIGEST=<transaction-digest> \
cargo test -p iota-lite-poc --test grpc_transaction_proof \
  builds_and_verifies_transaction_proof_from_localnet_grpc -- --ignored
```

## Current Limits

- The POC is transaction-proof only.
- The localnet test assumes the gRPC node is trusted for committee data.
- gRPC can only serve data the node still has available. For pruned history,
  archive or another historical source may still be needed.
- This crate is experimental and deliberately separate from the existing
  `iota-light-client` implementation.
