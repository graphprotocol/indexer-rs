# Changelog







## [1.6.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.7...indexer-service-rs-v1.6.0) (2025-07-02)


### Features

* support horizon ([#752](https://github.com/graphprotocol/indexer-rs/issues/752)) ([e8474bb](https://github.com/graphprotocol/indexer-rs/commit/e8474bb265ecce8d8be9859ae2b2044c6d2224b0))


### Bug Fixes

* **integration-tests:** fund v1 and v2 escrow in test setup ([e8474bb](https://github.com/graphprotocol/indexer-rs/commit/e8474bb265ecce8d8be9859ae2b2044c6d2224b0))

## [1.5.7](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.6...indexer-service-rs-v1.5.7) (2025-05-29)

## [1.5.6](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.5...indexer-service-rs-v1.5.6) (2025-05-28)


### Bug Fixes

* trigger release for recent dependency updates ([#730](https://github.com/graphprotocol/indexer-rs/issues/730)) ([f3ca43f](https://github.com/graphprotocol/indexer-rs/commit/f3ca43f9780f24e8b62c5478ccadfb2f6b10cc00))

## [1.5.5](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.4...indexer-service-rs-v1.5.5) (2025-05-14)


### Bug Fixes

* add libsasl2-2 to images ([#719](https://github.com/graphprotocol/indexer-rs/issues/719)) ([ea168db](https://github.com/graphprotocol/indexer-rs/pull/720/commits/ea168dbe6845845fe0a6598ffcd57f872ef1bdb2))
* **indexer:** propagate database errors back to the caller ([#715](https://github.com/graphprotocol/indexer-rs/issues/715)) ([0e85079](https://github.com/graphprotocol/indexer-rs/commit/0e850794f53457e0fdeb7b9a70a4e1332914d23e))

## [1.5.4](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.3...indexer-service-rs-v1.5.4) (2025-05-09)


### Bug Fixes

* **tap:** fix unused v2 deny list check ([#711](https://github.com/graphprotocol/indexer-rs/issues/711)) ([ab4aaba](https://github.com/graphprotocol/indexer-rs/commit/ab4aaba294c417e94766cdaaac8efda89509b544))

## [1.5.3](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.2...indexer-service-rs-v1.5.3) (2025-04-29)


### Bug Fixes

* update tap, axum, tonic, alloy dependencies: ([#702](https://github.com/graphprotocol/indexer-rs/issues/702)) ([6e49c0c](https://github.com/graphprotocol/indexer-rs/commit/6e49c0ceb61c0a972d36237cfc365dd7af72e9e3))

## [1.5.2](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.1...indexer-service-rs-v1.5.2) (2025-04-22)


### Bug Fixes

* Temporarily disable Horizon, enhance agent stability and logging ([#691](https://github.com/graphprotocol/indexer-rs/issues/691)) ([175ec75](https://github.com/graphprotocol/indexer-rs/commit/175ec75e5d675eb382a8fa3ced670aa28cdcb577))

## [1.5.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.5.0...indexer-service-rs-v1.5.1) (2025-04-10)


### Bug Fixes

* use the correct eip712 domain for each part of dips ([#667](https://github.com/graphprotocol/indexer-rs/issues/667)) ([ebe950b](https://github.com/graphprotocol/indexer-rs/commit/ebe950b7397a2844117f077df7709786cf3bb404))
* validate dips pricing ([#675](https://github.com/graphprotocol/indexer-rs/issues/675)) ([3d2728d](https://github.com/graphprotocol/indexer-rs/commit/3d2728da9f10a61815cc284ecb0f572d36394aee))

## [1.5.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.4.2...indexer-service-rs-v1.5.0) (2025-03-17)


### Features

* Escrow based signer validation ([#635](https://github.com/graphprotocol/indexer-rs/issues/635)) ([454c925](https://github.com/graphprotocol/indexer-rs/commit/454c925f184fbd8b454ef9aaea86fc53a218fcea))


### Bug Fixes

* skip processing of receipts when there's no receipt for a version ([#653](https://github.com/graphprotocol/indexer-rs/issues/653)) ([811567e](https://github.com/graphprotocol/indexer-rs/commit/811567eb0e227a84afbb9d5c438074b4a6ad03ae))

## [1.4.2](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.4.1...indexer-service-rs-v1.4.2) (2025-02-12)


### Bug Fixes

* allow responses for free queries with no allocation open ([#622](https://github.com/graphprotocol/indexer-rs/issues/622)) ([bce155e](https://github.com/graphprotocol/indexer-rs/commit/bce155e8515bd4e8a666a42d621a82776727a8cd))

## [1.4.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-service-rs-v1.4.0...indexer-service-rs-v1.4.1) (2025-01-16)


### Bug Fixes

* invalid serialization of CheckHealthError ([#565](https://github.com/graphprotocol/indexer-rs/issues/565)) ([73fdc95](https://github.com/graphprotocol/indexer-rs/commit/73fdc958aa93d89b8f58f3a93d859236d59ba20e))
* retain headers in attestation middleware ([#577](https://github.com/graphprotocol/indexer-rs/issues/577)) ([0d76f35](https://github.com/graphprotocol/indexer-rs/commit/0d76f35c70b252f0df34d919a43785f3c860dc01))

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
