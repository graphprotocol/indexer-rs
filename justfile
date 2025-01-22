# Set DATABASE_URL:
# export DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432
#
#  # print the help
help:
    just -l

deps:
    cargo install sqlx-cli --no-default-features --features native-tls,postgres

url:
    @echo export DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432

test: 
    cargo run --bin mock_tap_aggregator_server_runner & cargo test

clippy:
    cargo clippy --all-targets --all-features

sqlx-prepare:
    cargo sqlx prepare --workspace  -- --all-targets --all-features

psql-up: 
    @docker run -d --name indexer-rs-psql -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres
    @sleep 5
    @just migrate
     
psql-down:
    docker stop indexer-rs-psql
    docker rm indexer-rs-psql

migrate:
     sqlx migrate run --database-url postgresql://postgres:postgres@127.0.0.1:5432
