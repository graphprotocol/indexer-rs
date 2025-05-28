# Changelog

## [1.3.2](https://github.com/graphprotocol/indexer-rs/compare/indexer-config-v1.3.1...indexer-config-v1.3.2) (2025-05-28)


### Bug Fixes

* trigger release for recent dependency updates ([#730](https://github.com/graphprotocol/indexer-rs/issues/730)) ([f3ca43f](https://github.com/graphprotocol/indexer-rs/commit/f3ca43f9780f24e8b62c5478ccadfb2f6b10cc00))

## [1.3.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-config-v1.3.0...indexer-config-v1.3.1) (2025-04-10)


### Bug Fixes

* **config:** add missing config field ([26a6f56](https://github.com/graphprotocol/indexer-rs/commit/26a6f5680a6bfefb9ae82e3d08c2562179780a10))
* validate dips pricing ([#675](https://github.com/graphprotocol/indexer-rs/issues/675)) ([3d2728d](https://github.com/graphprotocol/indexer-rs/commit/3d2728da9f10a61815cc284ecb0f572d36394aee))

## [1.3.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-config-v1.2.2...indexer-config-v1.3.0) (2025-02-12)


### Features

* add escape hatch to trusted senders ([#621](https://github.com/graphprotocol/indexer-rs/issues/621)) ([bdc40ef](https://github.com/graphprotocol/indexer-rs/commit/bdc40ef33ee0b9b063ca8eeaa5374ef6f4779626))
* add option to avoid denying senders ([#607](https://github.com/graphprotocol/indexer-rs/issues/607)) ([d0731e3](https://github.com/graphprotocol/indexer-rs/commit/d0731e3bf2e36f587e9318be9028897a0a35cda4))

## [1.2.2](https://github.com/graphprotocol/indexer-rs/compare/indexer-config-v1.2.1...indexer-config-v1.2.2) (2024-12-18)


### Bug Fixes

* add receipts timeout config ([#537](https://github.com/graphprotocol/indexer-rs/issues/537)) ([2438895](https://github.com/graphprotocol/indexer-rs/commit/243889570d2a2146816a23dab3bfe39e79e5e010))

## [1.2.1](https://github.com/graphprotocol/indexer-rs/compare/indexer-config-v1.2.0...indexer-config-v1.2.1) (2024-11-08)


### Bug Fixes

* ignore empty environment variables strings ([#473](https://github.com/graphprotocol/indexer-rs/issues/473)) ([1bc3c4e](https://github.com/graphprotocol/indexer-rs/commit/1bc3c4e96584ef8977a133e03530cdcb801d2270))

## [1.2.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-config-v1.1.0...indexer-config-v1.2.0) (2024-10-09)


### Features

* shared env prefix for service and tap ([#356](https://github.com/graphprotocol/indexer-rs/issues/356)) ([5ff0550](https://github.com/graphprotocol/indexer-rs/commit/5ff05500d86d04a4cbe53fe3c724404585e7647a))

## [1.1.0](https://github.com/graphprotocol/indexer-rs/compare/indexer-config-v1.0.0...indexer-config-v1.1.0) (2024-10-09)


### Features

* Accept config to be through file or env vars ([#352](https://github.com/graphprotocol/indexer-rs/issues/352)) ([9e44ad4](https://github.com/graphprotocol/indexer-rs/commit/9e44ad4fd04477e07dba4776f4a2de8a338f0f61))
* **config:** postgres_url or splitten components ([#339](https://github.com/graphprotocol/indexer-rs/issues/339)) ([2b9adcf](https://github.com/graphprotocol/indexer-rs/commit/2b9adcfa2cc3f4bc9024fb3604d0c85104a080d4))
* **config:** use env vars in config files ([#344](https://github.com/graphprotocol/indexer-rs/issues/344)) ([1db3adb](https://github.com/graphprotocol/indexer-rs/commit/1db3adb12325ffd75bc031fa6299031357eeb60a))


### Bug Fixes

* Add max_willing_to_lose default value ([#315](https://github.com/graphprotocol/indexer-rs/issues/315)) ([33f449a](https://github.com/graphprotocol/indexer-rs/commit/33f449acf55470e5bfe9713d8dcd424f79a7b702))
* add warn where trigger_value is below 0.1 grt ([#340](https://github.com/graphprotocol/indexer-rs/issues/340)) ([203e1ec](https://github.com/graphprotocol/indexer-rs/commit/203e1ec1f244467d944f8f0a02a653c05bf6105d))
