# Changelog

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
