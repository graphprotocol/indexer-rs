## Local-Network Overrides (User Guide)

This note explains **what our local setup does differently** from upstream `edgeandnode/local-network` and how to reproduce it without upstreaming yet.

### Why these overrides exist
- **Gateway stability**: the network subgraph sometimes returns `null` addresses during boot. We guard against that so the gateway can start.
- **Multi‑mnemonic testing**: to simulate multiple operators / allocations without fighting a single mnemonic.
- **Trusted senders testing**: to allow multiple senders to exceed escrow limits in local tests.

---

## What’s different and how to use it

### 1) Gateway starts even when dispute manager is missing
If the network subgraph returns `null` for dispute managers, the gateway will now fall back to safe defaults so it doesn’t crash during boot.

**Outcome:** gateway reliably starts in local-network, even during early boot or partial indexing.

---

### 2) Multiple operator mnemonics (local testing)
You can now provide **additional operator mnemonics** in local-network to test multiple allocation identities.

Set this before running `just setup`:

```bash
export INDEXER_MNEMONICS="mnemonic one..., mnemonic two..."
```

**Outcome:** indexer-service and tap-agent will both load multiple mnemonics for signing and validation.

---

### 3) Trusted senders (local testing)
You can now provide multiple trusted senders in local-network to simulate multiple gateway payers.

Set this before running `just setup`:

```bash
export TRUSTED_SENDERS="0xabc...,0xdef..."
```

**Outcome:** those senders can exceed escrow balance up to `max_amount_willing_to_lose_grt` in tests.

---

## Quick sanity checks

- Gateway should boot cleanly (no `invalid string length` panic).
- `indexer-service` and `tap-agent` configs should show the extra mnemonics/senders in their generated `config.toml`.

---

## Notes on upstreaming
This document is a **temporary reference** so we can reproduce the behavior locally without having to upstream right now. If we decide to upstream later, we’ll turn these into clean patches with proper reviews.
