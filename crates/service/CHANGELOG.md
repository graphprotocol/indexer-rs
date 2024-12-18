# Changelog







## [1.4.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.3.2...indexer-service-rs-v1.4.0) (2024-12-18)


### Features

* add graph-indexed header in subgraph query response ([#522](https://github.com/graphprotocol/indexer-rs/issues/522)) ([a0d719f](https://github.com/graphprotocol/indexer-rs/commit/a0d719f1a0834ff5ba99522fadd6b52c079425d3)), closes [#494](https://github.com/graphprotocol/indexer-rs/issues/494)
* add request status code for handler metric ([59eed9f](https://github.com/graphprotocol/indexer-rs/commit/59eed9f347458fdae0798f12e500533c302a9c40))


### Bug Fixes

* error in order for middleware router ([12dc95c](https://github.com/graphprotocol/indexer-rs/commit/12dc95c11fe0b6f8e82bfc7604f8079a5010e414))
* update status code for errors ([4e05c90](https://github.com/graphprotocol/indexer-rs/commit/4e05c90537ffd35656a10a652b2ae5678ed026d8))

## [1.3.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.3.0...indexer-service-rs-v1.3.1) (2024-11-06)


### Bug Fixes

* use request from gateway instead of serde req ([#464](https://github.com/graphprotocol/indexer-rs/issues/464)) ([fdeda9f](https://github.com/graphprotocol/indexer-rs/commit/fdeda9fea996f96e1c0a7bef291a551f426f5591))

## [1.3.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.2.2...indexer-service-rs-v1.3.0) (2024-11-05)


### Features

* add versioning on cli ([#460](https://github.com/graphprotocol/indexer-rs/issues/460)) ([419e7ff](https://github.com/graphprotocol/indexer-rs/commit/419e7ff513fd11294c8523f5dae102a5cbf77f94))

## [1.2.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.1.1...indexer-service-rs-v1.2.0) (2024-10-30)


### Features

* add value check ([#153](https://github.com/graphprotocol/indexer-rs/issues/153)) ([1e4a3cd](https://github.com/graphprotocol/indexer-rs/commit/1e4a3cdd8c18b5356e64285b8082d8abde20d6de))

## [1.1.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.1.0...indexer-service-rs-v1.1.1) (2024-10-09)


### Bug Fixes

* use INFO as default level for logs ([#353](https://github.com/graphprotocol/indexer-rs/issues/353)) ([20b959d](https://github.com/graphprotocol/indexer-rs/commit/20b959d4d2095a0d9b545b8c25be7259ac387f12))

## [1.1.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.0.0...indexer-service-rs-v1.1.0) (2024-10-09)


### Features

* Accept config to be through file or env vars ([#352](https://github.com/graphprotocol/indexer-rs/issues/352)) ([9e44ad4](https://github.com/graphprotocol/indexer-rs/commit/9e44ad4fd04477e07dba4776f4a2de8a338f0f61))
* add metrics to service ([#319](https://github.com/graphprotocol/indexer-rs/issues/319)) ([57c89e2](https://github.com/graphprotocol/indexer-rs/commit/57c89e237a57b49214eaf902303e3d89c9d82396))
* **config:** postgres_url or splitten components ([#339](https://github.com/graphprotocol/indexer-rs/issues/339)) ([2b9adcf](https://github.com/graphprotocol/indexer-rs/commit/2b9adcfa2cc3f4bc9024fb3604d0c85104a080d4))
