# Changelog





## [1.9.3](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.9.2...indexer-tap-agent-v1.9.3) (2025-04-22)


### Bug Fixes

* Temporarily disable Horizon, enhance agent stability and logging ([#691](https://github.com/graphprotocol/indexer-rs/issues/691)) ([175ec75](https://github.com/graphprotocol/indexer-rs/commit/175ec75e5d675eb382a8fa3ced670aa28cdcb577))

## [1.9.2](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.9.1...indexer-tap-agent-v1.9.2) (2025-04-10)


### Bug Fixes

* **config:** add missing config field ([26a6f56](https://github.com/graphprotocol/indexer-rs/commit/26a6f5680a6bfefb9ae82e3d08c2562179780a10))

## [1.9.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.9.0...indexer-tap-agent-v1.9.1) (2025-02-13)


### Bug Fixes

* send allocation creation message to correct sender account ([387d660](https://github.com/graphprotocol/indexer-rs/commit/387d660a7a9a4f2a669762408f872e36251a3855))

## [1.9.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.8.0...indexer-tap-agent-v1.9.0) (2025-02-12)


### Features

* add escape hatch to trusted senders ([#621](https://github.com/graphprotocol/indexer-rs/issues/621)) ([bdc40ef](https://github.com/graphprotocol/indexer-rs/commit/bdc40ef33ee0b9b063ca8eeaa5374ef6f4779626))
* add option to avoid denying senders ([#607](https://github.com/graphprotocol/indexer-rs/issues/607)) ([d0731e3](https://github.com/graphprotocol/indexer-rs/commit/d0731e3bf2e36f587e9318be9028897a0a35cda4))


### Performance Improvements

* speed up startup setup queries ([#625](https://github.com/graphprotocol/indexer-rs/issues/625)) ([c5b115e](https://github.com/graphprotocol/indexer-rs/commit/c5b115e997a71582dad7d75c4c954cece225fdaa))

## [1.8.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.7.4...indexer-tap-agent-v1.8.0) (2025-01-29)


### Features

* grpc client for tap aggregator ([#583](https://github.com/graphprotocol/indexer-rs/issues/583)) ([c3ede8f](https://github.com/graphprotocol/indexer-rs/commit/c3ede8f8bfe1820dd0bdc8876af083a8971d39ff))

## [1.7.4](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.7.3...indexer-tap-agent-v1.7.4) (2024-12-18)


### Bug Fixes

* add receipts timeout config ([#537](https://github.com/graphprotocol/indexer-rs/issues/537)) ([2438895](https://github.com/graphprotocol/indexer-rs/commit/243889570d2a2146816a23dab3bfe39e79e5e010))
* shutdown tap-agent if db connection lost ([#520](https://github.com/graphprotocol/indexer-rs/issues/520)) ([16b42e2](https://github.com/graphprotocol/indexer-rs/commit/16b42e2c41d9c0bbe5d6e187eea49c79cdeac8d9))
* start listening to messages after start up ([#518](https://github.com/graphprotocol/indexer-rs/issues/518)) ([7e5d280](https://github.com/graphprotocol/indexer-rs/commit/7e5d28085402fdd9110ab61e4fc4b6d767ab9fff))

## [1.7.3](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.7.2...indexer-tap-agent-v1.7.3) (2024-11-13)


### Bug Fixes

* remove escrow adapter check ([#483](https://github.com/graphprotocol/indexer-rs/issues/483)) ([1faa61e](https://github.com/graphprotocol/indexer-rs/commit/1faa61e3432cf94771f29ab0872572eeb8aabd06))

## [1.7.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.6.0...indexer-tap-agent-v1.7.0) (2024-11-05)


### Features

* add versioning on cli ([#460](https://github.com/graphprotocol/indexer-rs/issues/460)) ([419e7ff](https://github.com/graphprotocol/indexer-rs/commit/419e7ff513fd11294c8523f5dae102a5cbf77f94))

## [1.6.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.5.0...indexer-tap-agent-v1.6.0) (2024-11-04)


### Features

* calculate unnagregated fees even if the rav fails ([#442](https://github.com/graphprotocol/indexer-rs/issues/442)) ([3ae39c0](https://github.com/graphprotocol/indexer-rs/commit/3ae39c0b91d9798a2260736358ee3ef45d6944b9))


### Bug Fixes

* change buffered to buffered_unordened for faster collecting ([#457](https://github.com/graphprotocol/indexer-rs/issues/457)) ([7bf77c5](https://github.com/graphprotocol/indexer-rs/commit/7bf77c574ba9316d647aad5877f91db45b142c43))

## [1.5.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.4.1...indexer-tap-agent-v1.5.0) (2024-11-01)


### Features

* add sender fee tracker metric ([f4fb366](https://github.com/graphprotocol/indexer-rs/commit/f4fb36610a438a8c9e1ee73da44c369f247843d1))


### Bug Fixes

* recalculate all allocations ([09993b7](https://github.com/graphprotocol/indexer-rs/commit/09993b748ded427f485b6979a5ce342299d84dba))
* use requesting value in global ([1ea3afa](https://github.com/graphprotocol/indexer-rs/commit/1ea3afae44800d467d27c8474612580f1ca2bee0))

## [1.4.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.4.0...indexer-tap-agent-v1.4.1) (2024-10-30)


### Bug Fixes

* check subgraph before closing allocations ([#435](https://github.com/graphprotocol/indexer-rs/issues/435)) ([a3cc07e](https://github.com/graphprotocol/indexer-rs/commit/a3cc07e72d2e6a8440788c96ff005ea566eac751))

## [1.4.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.3.0...indexer-tap-agent-v1.4.0) (2024-10-30)


### Features

* add value check ([#153](https://github.com/graphprotocol/indexer-rs/issues/153)) ([1e4a3cd](https://github.com/graphprotocol/indexer-rs/commit/1e4a3cdd8c18b5356e64285b8082d8abde20d6de))


### Bug Fixes

* initialize allocations monitor ([#428](https://github.com/graphprotocol/indexer-rs/issues/428)) ([f0c7d03](https://github.com/graphprotocol/indexer-rs/commit/f0c7d0367abf8a833f1824264780df50ad6a3c52))
* refresh database before closing allocation ([#411](https://github.com/graphprotocol/indexer-rs/issues/411)) ([edc1d2c](https://github.com/graphprotocol/indexer-rs/commit/edc1d2c733ca7a9ac57656066cf3f701ac76df4f))


### Performance Improvements

* create allocations in parallel ([#431](https://github.com/graphprotocol/indexer-rs/issues/431)) ([a0ed7b2](https://github.com/graphprotocol/indexer-rs/commit/a0ed7b25355c4655805b686f369f379799f64718))

## [1.3.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.2.1...indexer-tap-agent-v1.3.0) (2024-10-23)


### Features

* add counter trigger for rav request ([#384](https://github.com/graphprotocol/indexer-rs/issues/384)) ([06644c5](https://github.com/graphprotocol/indexer-rs/commit/06644c5ad934db816725fb5e95bed9ab3c98d536))


### Bug Fixes

* add backoff if could not find heaviest allocation ([#397](https://github.com/graphprotocol/indexer-rs/issues/397)) ([f9b64f3](https://github.com/graphprotocol/indexer-rs/commit/f9b64f39f5d57fd520d83cf4ded1f20d29a349db))
* limit the max concurrent rav requests ([#396](https://github.com/graphprotocol/indexer-rs/issues/396)) ([b4c876d](https://github.com/graphprotocol/indexer-rs/commit/b4c876d42432119b70b7c445155444ea1a9e3ba0))

## [1.2.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.2.0...indexer-tap-agent-v1.2.1) (2024-10-18)


### Bug Fixes

* calculate unaggregate receipts up to last_id ([#385](https://github.com/graphprotocol/indexer-rs/issues/385)) ([76873f9](https://github.com/graphprotocol/indexer-rs/commit/76873f95f35fe636759f47763cec27ddc6f23f31))

## [1.2.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.1.1...indexer-tap-agent-v1.2.0) (2024-10-17)


### Features

* Move backoff to to the tracker to remove errors ([#377](https://github.com/graphprotocol/indexer-rs/issues/377)) ([1bde9b4](https://github.com/graphprotocol/indexer-rs/commit/1bde9b4e660ecd175bff427fac06e04f3985a8f8))


### Performance Improvements

* trigger rav request concurrently ([#381](https://github.com/graphprotocol/indexer-rs/issues/381)) ([bb7b7c6](https://github.com/graphprotocol/indexer-rs/commit/bb7b7c678cdc20bab805c3eacfd4aedf99a868b5))
* use latest_rav to recalculate fees ([#379](https://github.com/graphprotocol/indexer-rs/issues/379)) ([7a45c26](https://github.com/graphprotocol/indexer-rs/commit/7a45c260c4d4961171bb67db938d446cbe5d891c))

## [1.1.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.1.0...indexer-tap-agent-v1.1.1) (2024-10-09)


### Bug Fixes

* use INFO as default level for logs ([#353](https://github.com/graphprotocol/indexer-rs/issues/353)) ([20b959d](https://github.com/graphprotocol/indexer-rs/commit/20b959d4d2095a0d9b545b8c25be7259ac387f12))

## [1.1.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-tap-agent-v1.0.0...indexer-tap-agent-v1.1.0) (2024-10-09)


### Features

* Accept config to be through file or env vars ([#352](https://github.com/graphprotocol/indexer-rs/issues/352)) ([9e44ad4](https://github.com/graphprotocol/indexer-rs/commit/9e44ad4fd04477e07dba4776f4a2de8a338f0f61))
* Add a warning in case senders are undenied manually from db ([#346](https://github.com/graphprotocol/indexer-rs/issues/346)) ([00af506](https://github.com/graphprotocol/indexer-rs/commit/00af5068486c23d0aa4eddc59a18da0335955e8c))
* add tracker for buffer unaggregated fees ([#324](https://github.com/graphprotocol/indexer-rs/issues/324)) ([676a437](https://github.com/graphprotocol/indexer-rs/commit/676a4374e2f27b3a0225c6316360c4366776fdae))
* **config:** postgres_url or splitten components ([#339](https://github.com/graphprotocol/indexer-rs/issues/339)) ([2b9adcf](https://github.com/graphprotocol/indexer-rs/commit/2b9adcfa2cc3f4bc9024fb3604d0c85104a080d4))


### Bug Fixes

* Add more information if rav request is timing out ([#325](https://github.com/graphprotocol/indexer-rs/issues/325)) ([5edf6cf](https://github.com/graphprotocol/indexer-rs/commit/5edf6cfa31900fd3b99ff9a7e586501d7a6a281c))
* Store receipt error into db ([#322](https://github.com/graphprotocol/indexer-rs/issues/322)) ([212e06a](https://github.com/graphprotocol/indexer-rs/commit/212e06a606691dd47635d2b6643b706dd1f958e7))
* **tap-agent:** bulk insert of failed receipts ([#329](https://github.com/graphprotocol/indexer-rs/issues/329)) ([f65d95c](https://github.com/graphprotocol/indexer-rs/commit/f65d95c3122a87d6a06837efe5f46a53ab8f731f))
* Use dashboard metric for Rav trigger ([#317](https://github.com/graphprotocol/indexer-rs/issues/317)) ([c693f0e](https://github.com/graphprotocol/indexer-rs/commit/c693f0ebe36a0f5dce8f46fd974eef1a5924c3c6))
