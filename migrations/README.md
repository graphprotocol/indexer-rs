# Database Migration setup

Do not run migrations from `indexer-rs` binaries; use `indexer-agent` to sync and migrate the database.

Use the migration files here for local testing only. The source of truth lives in the `graphprotocol/indexer` repo.

### Prerequisite: Install sqlx-cli

Run `cargo install sqlx-cli --no-default-features --features native-tls,postgres` 

Simple option: run general installation that supports all databases supported by SQLx
`cargo install sqlx-cli`

### Run the migration

Run `sqlx migrate run`
