# Proposal: Proof of Inclusion

## Summary

This proposal recommends rewriting `iota-light-client` into a **Proof of Inclusion** package.

The existing light client implementation is correct for the model it was built around. It verifies targeted chain claims without running a full node, and it uses a trusted genesis anchor plus authenticated end-of-epoch checkpoints to advance committee trust.

The proposed change is not a correction of the old design. It is a product and architecture update enabled by recently available committee information from the node. Since committee history can now be exposed from genesis onward, the package no longer needs to operate primarily as a stateful checkpoint-syncing light client for proof construction.

The new package should focus on constructing and verifying portable proofs that specific transactions, objects, or events are included in authenticated chain data.

## Current Model

The current `iota-light-client` works as a stateful verifier.

It uses:

- a genesis blob as the initial trust anchor
- checkpoint summary storage
- end-of-epoch checkpoint syncing
- local committee walking
- full checkpoint data for proof construction
- local verification of constructed proofs

The current trust path is:

```text
genesis.blob
-> epoch 0 committee
-> verify end-of-epoch checkpoint for epoch 0
-> extract epoch 1 committee
-> verify end-of-epoch checkpoint for epoch 1
-> repeat until the target epoch
```

This is a valid light-client model. It avoids trusting a full node for the committee chain because each committee transition is verified locally.

Proof construction depends on checkpoint data. For object and event proofs, the client resolves the target checkpoint, downloads the full checkpoint, scans it for the target transaction, and packages the required witness data into a proof.

## Why This Can Change Now

The key change is committee availability. Committee information can now be served by the node from genesis onward. This was the missing capability that made the original checkpoint-sync model necessary for the light client.

Relevant changes include:

### IOTA Monorepo

- `d1623576a6` - Add `GetEpoch` gRPC endpoint
- `1bede6aa14` - Add `GetServiceInfo` gRPC endpoint
- `15dfdcfd3a` - Add checkpoint gRPC endpoints

### IOTA Rust SDK

- `0bb2128` - Add gRPC client, types, and proto-build

With committee history available from the node, the package can separate two concerns more cleanly:

- **Proof construction:** fetch and package witness data from a node or historical data source.
- **Proof verification:** cryptographically verify the packaged proof against an authenticated committee.

This does not change the cryptographic trust model. It changes how proof inputs are sourced.

## Proposed Product

The package should no longer be presented as a general light client.

A light client usually implies that the client maintains enough local state to track the chain in a reduced form. The proposed product does not need to behave that way. It should instead answer targeted inclusion questions:

- Did this transaction execute?
- Was this object version produced by authenticated chain state?
- Did this event occur?
- Is this committee transition authenticated?

The proposed product name is:

```text
Proof of Inclusion
```

This name matches the package responsibility: construct and verify inclusion proofs for specific chain claims.

## Package Components

The Proof of Inclusion package should contain separate construction and verification components.

- **Proof constructor:** Fetches witness data from a node, indexer, archive-backed source, or another configured data source, then packages that data into a portable proof.
- **Proof verifier:** Locally verifies the packaged proof against an authenticated committee. The verifier is a local package component, not an external network participant.
- **Committee resolver:** Gets a committee through an explicit trust mode, such as trusted-node lookup or genesis-anchored walking.
- **Committee verifier:** Authenticates committee transitions from the trusted genesis committee through end-of-epoch checkpoint summaries.
- **Committee cache:** Stores only verified committee lineage data so later verification can resume from a known trusted point.

These components can be exposed through Rust libraries first, CLI commands such as `create-proof` and `verify-proof`, and later WASM bindings for browser or wallet environments.

## Proposed Architecture

The proposed architecture uses node-provided or archive-provided proof inputs.

A proof input source can serve:

- chain ID and current epoch
- epoch information
- committee information from genesis onward
- last checkpoint of each epoch
- certified checkpoint summaries
- checkpoint signatures
- checkpoint contents when required
- transaction data
- transaction effects
- events when present
- object-to-transaction linkage when required

The Proof of Inclusion package packages this data into a portable proof and verifies the proof locally.

The data source provides proof inputs. The PoI verifier locally decides whether those inputs are trustworthy.

## Construction Flow

For a transaction proof, construction should follow this shape:

```text
transaction digest
-> fetch transaction data, effects, events, and checkpoint sequence
-> fetch checkpoint summary, signature, and contents
-> package proof
```

For an object proof, construction should follow this shape:

```text
object ID and version
-> resolve the transaction that created or last mutated that object version
-> reuse the transaction proof path
-> include the object target in the proof
```

For an event proof, construction should follow this shape:

```text
event ID
-> resolve the transaction and event sequence
-> reuse the transaction proof path
-> include the event target in the proof
```

The proof is not trusted merely because it was constructed. It becomes trusted only after local verification succeeds.

## Verification Flow

Verification remains local and cryptographic.

The proof verifier checks:

- the checkpoint summary is certified by the expected committee
- the checkpoint contents match the certified summary
- the transaction digest is included in the checkpoint contents
- the transaction, effects, and events are internally consistent
- the requested object or event target matches the authenticated transaction data

Verification can run offline once the proof and the required committee anchor are available.

Proofs are tamper-evident. Any modification to authenticated proof fields, such as transaction data, effects, events, checkpoint contents, checkpoint summary, target object, or target event, causes verification to fail because the modified data no longer matches the hashes and signatures committed by the certified checkpoint.

## Committee Trust

The package should support explicit committee trust modes.

### Trusted Node Mode

In trusted node mode, the verifier fetches the committee directly from the node.

This mode is useful for:

- localnet
- testing
- trusted infrastructure
- application environments where the node is already trusted

This mode is ergonomic, but it does not provide independent verification of committee lineage.

### Anchored Verification Mode

In anchored verification mode, the verifier starts from a trusted genesis blob and walks forward epoch by epoch.

The flow is:

```text
genesis.blob
-> trusted committee for epoch 0
-> fetch last checkpoint of epoch 0
-> fetch certified checkpoint summary and signature
-> verify checkpoint with committee 0
-> extract committee 1
-> repeat until target epoch
```

This preserves the independent verification model. The data source serves checkpoint and committee transition data, but the verifier only accepts the next committee after validating the previous committee's signatures.

The command-line flow should use a genesis blob as the trusted anchor.

## Committee Cache

Walking from genesis can use a cache layer.

Without caching, every verification for a later epoch repeats the same work from epoch 0 to the target epoch. That is correct, but unnecessarily expensive.

The package should cache only verified committee lineage data:

```text
epoch -> verified committee
epoch -> verified end-of-epoch checkpoint summary
```

The verified committee is the practical reusable state. The verified end-of-epoch checkpoint summary is the evidence that produced the next committee.

This allows the verifier to resume from the latest trusted cached committee rather than starting from genesis every time.

The cache should be an abstraction, not a hard dependency on local disk storage. Recommended implementations are:

- in-memory cache by default
- optional file-backed cache for CLI usage
- optional persistent cache for backend services
- optional browser-compatible storage later

Cache entries should not become an implicit trust source. The package should only write committees and transition checkpoints after they have been cryptographically verified.

## Historical Data Availability

Proof construction depends on the selected data source being able to serve the requested historical data.

If a full node has pruned old transactions, checkpoint contents, effects, or events, then construction for those old targets can fail from that node. This does not break verification. It only means the constructor needs another data source.

For historical proofs, construction may use an archive-backed historical data source. This source is not part of the trust model. It only supplies transaction, effects, events, checkpoint summary, and checkpoint contents data.

The PoI verifier must still authenticate:

- the committee chain from genesis or a cached verified committee
- the certified checkpoint summary
- the checkpoint contents
- the requested transaction, event, or object claim

Archive access solves data availability. PoI verification preserves local cryptographic trust.

The current IOTA archival format is useful for transaction and event proof construction because it stores transaction history, effects, events, and checkpoints. Object-version proofs may need an indexer or another store that retains historical object versions, because the current archive documentation states that historical object versions are not included in the archival format.

Archive-backed construction should be treated as a planned data-source integration. The current proof-of-concept builds proof inputs from a gRPC full node.

## What Changes

The package changes from a stateful light-client utility into a Proof of Inclusion package.

The new package should:

- construct proofs from configured proof input sources
- verify proofs locally
- keep proof construction separate from proof verification
- make committee trust mode explicit
- support anchored committee walking from a genesis blob
- avoid requiring local checkpoint summary sync for normal construction
- support optional caching as an implementation detail
- support archive-backed construction as a future data-source integration

## What Does Not Change

The core verification model remains.

The package still verifies authenticated chain claims using:

- certified checkpoint summaries
- checkpoint contents
- transaction digests
- transaction effects
- events
- committee signatures
- committee transitions

The rewrite changes the operating model and product shape. It does not remove the need for cryptographic verification.

## Illustrative Component Interface

The exact API surface should be finalized during implementation, but the intended separation of responsibilities could look like this:

```text
ProofConstructor
ProofVerifier
CommitteeResolver
CommitteeVerifier
CommitteeCache
```

Example methods:

```text
build_transaction_proof(digest)
build_object_proof(object_id, version)
build_event_proof(event_id)

committee_from_trusted_node(epoch)
committee_from_genesis_blob(path)
committee_from_trusted_anchor(anchor, target_epoch)

verify_proof(committee, proof)
```

This keeps the main distinction clear:

```text
construct proof = gather and package witness data
verify proof = cryptographically check packaged data
```
