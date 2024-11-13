# Database Migration setup

Indexer service binary does _NOT_ run database migrations automatically in the program binary, as it might introduce conflicts with the migrations run by indexer agent. Indexer agent is solely responsible for syncing and migrating the database. 

The migration files here are included here for testing only. 

### Prerequisite: Install sqlx-cli

Run `cargo install sqlx-cli --no-default-features --features native-tls,postgres` 

Simple option: run general installation that supports all databases supported by SQLx
`cargo install sqlx-cli`

### Run the migration

Run `sqlx migrate run`
