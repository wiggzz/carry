# Changelog

## [1.0.0](https://github.com/wiggzz/carry/compare/v0.6.0...v1.0.0) (2026-09-04)


### ⚠ BREAKING CHANGES

* streamline README onboarding ([#62](https://github.com/wiggzz/carry/issues/62))

### Features

* add model-directed context pressure reminders ([#71](https://github.com/wiggzz/carry/issues/71)) ([33d9882](https://github.com/wiggzz/carry/commit/33d9882bdcba20b877f8e3d7628bde283bdd7f20))
* add resumable web sessions and live run events ([#57](https://github.com/wiggzz/carry/issues/57)) ([e784bf1](https://github.com/wiggzz/carry/commit/e784bf17d8f78d65ede4899712b02eb90b80b123))
* add retained-context SWE-bench smoke ([#63](https://github.com/wiggzz/carry/issues/63)) ([d203546](https://github.com/wiggzz/carry/commit/d203546195bf4f063164277f2d18f4b9f59e3fa5))
* add selectable compaction policy ([#61](https://github.com/wiggzz/carry/issues/61)) ([57ec112](https://github.com/wiggzz/carry/commit/57ec112a13478b0b512250d1889c94941573516d))
* complete native retained-session support ([#67](https://github.com/wiggzz/carry/issues/67)) ([8729e9a](https://github.com/wiggzz/carry/commit/8729e9aa78152a4cbb132b87cace4aa4f10d26c1))
* prepare SWE-bench environments before agents ([#50](https://github.com/wiggzz/carry/issues/50)) ([3def41e](https://github.com/wiggzz/carry/commit/3def41e82146c10e1dad7ebf45650721d94b8c6b))
* publish reusable SWE-bench task images ([#53](https://github.com/wiggzz/carry/issues/53)) ([35d00fa](https://github.com/wiggzz/carry/commit/35d00fa23d4cbb4361f45659ea7ee38981b8fa85))
* revalidate protected context leases ([#75](https://github.com/wiggzz/carry/issues/75)) ([a544935](https://github.com/wiggzz/carry/commit/a544935828b61009f12fb7e312c9e9a8ab3f94b7))


### Bug Fixes

* authorize prepared image publishing and cleanup ([#55](https://github.com/wiggzz/carry/issues/55)) ([c3d3f2d](https://github.com/wiggzz/carry/commit/c3d3f2d05180fe7c20422549e17e8cfa10fb8b35))
* export Carry compaction policy to benchmark runner ([#70](https://github.com/wiggzz/carry/issues/70)) ([b941095](https://github.com/wiggzz/carry/commit/b9410959d0b8b6a25bf47cdfa08bccc832870579))
* extend official agent phase budget ([#60](https://github.com/wiggzz/carry/issues/60)) ([4c98efc](https://github.com/wiggzz/carry/commit/4c98efc4d3ca9e06676be4b2ae152855246cf6e7))
* forward Carry compaction policy to native CLI ([#68](https://github.com/wiggzz/carry/issues/68)) ([57567ad](https://github.com/wiggzz/carry/commit/57567ad9fd4b10fc52239325829f3ff6b2b64672))
* preserve prompt-cache affinity across native resumes ([#66](https://github.com/wiggzz/carry/issues/66)) ([12c0d61](https://github.com/wiggzz/carry/commit/12c0d61590397a3755db5d168090c99788798e9c))
* propagate Carry compaction policy through image command ([#69](https://github.com/wiggzz/carry/issues/69)) ([045ecdc](https://github.com/wiggzz/carry/commit/045ecdc336b27b7a5f03be66144bf311d3896771))
* scale official agent concurrency ([#59](https://github.com/wiggzz/carry/issues/59)) ([9659ae3](https://github.com/wiggzz/carry/commit/9659ae348621de758cd1d76095b70e071bf7302b))
* tolerate empty context reminder templates ([#72](https://github.com/wiggzz/carry/issues/72)) ([f6bf52b](https://github.com/wiggzz/carry/commit/f6bf52b99d3a42834677d93d3e7bf100692f1518))


### Reverts

* remove experimental context-pressure reminders ([#74](https://github.com/wiggzz/carry/issues/74)) ([314f05d](https://github.com/wiggzz/carry/commit/314f05dad6b2c8884faa0791af22e87181bb312d))


### Documentation

* streamline README onboarding ([#62](https://github.com/wiggzz/carry/issues/62)) ([0f05e22](https://github.com/wiggzz/carry/commit/0f05e224eb20ecc237d2f7df0970f489d728d36b))

## [0.6.0](https://github.com/wiggzz/carry/compare/v0.5.0...v0.6.0) (2026-08-22)


### Features

* offload large shell output ([#41](https://github.com/wiggzz/carry/issues/41)) ([dcff860](https://github.com/wiggzz/carry/commit/dcff8606d431cd43141b1687c5d3380e5f53935f))
* plan compaction across cache generations ([#37](https://github.com/wiggzz/carry/issues/37)) ([f2ac9c7](https://github.com/wiggzz/carry/commit/f2ac9c785bed8f8d240619515cb4835838d26505))


### Bug Fixes

* bound official evaluator concurrency ([#44](https://github.com/wiggzz/carry/issues/44)) ([d3e2bff](https://github.com/wiggzz/carry/commit/d3e2bff55726b66d6fb24f488ef5024c42f139b2))
* handle historical Git metadata in benchmark preflight ([#48](https://github.com/wiggzz/carry/issues/48)) ([441760e](https://github.com/wiggzz/carry/commit/441760e081e841d29a9286150232dd2c5caea4c2))
* remove protected benchmark step cap ([#40](https://github.com/wiggzz/carry/issues/40)) ([bde8fe3](https://github.com/wiggzz/carry/commit/bde8fe316e95b203184c57c0c8beb6e0512cff4a))
* validate pull request titles ([#49](https://github.com/wiggzz/carry/issues/49)) ([8460022](https://github.com/wiggzz/carry/commit/8460022070d0bd2bbc13cde55263642376f0af58))

## [0.5.0](https://github.com/wiggzz/carry/compare/v0.4.0...v0.5.0) (2026-08-17)


### Features

* add interactive CLI sessions ([#35](https://github.com/wiggzz/carry/issues/35)) ([70f8516](https://github.com/wiggzz/carry/commit/70f851680deadc4c3bc10f4754dfc3f04baa3267))


### Performance Improvements

* cache SWE-bench instance images ([#38](https://github.com/wiggzz/carry/issues/38)) ([8fc226d](https://github.com/wiggzz/carry/commit/8fc226d85b067ece830c0c4ac1ec567bc826ce1b))

## [0.4.0](https://github.com/wiggzz/carry/compare/v0.3.0...v0.4.0) (2026-08-16)


### Features

* **benchmarks:** add live progress and performance reports ([#33](https://github.com/wiggzz/carry/issues/33)) ([d9e7c5f](https://github.com/wiggzz/carry/commit/d9e7c5f1f79f69e3aa540beaffca74767e12a386))

## [0.3.0](https://github.com/wiggzz/carry/compare/v0.2.1...v0.3.0) (2026-08-16)


### Features

* add protected official benchmark mode ([99e3a69](https://github.com/wiggzz/carry/commit/99e3a69d944ef631d3570487400ac37d9c2b2f0e))
* add protected official benchmark mode ([5b6ba0a](https://github.com/wiggzz/carry/commit/5b6ba0a8d55f4e555c3f71f7ae1ba1f95bc674fc))
* add protected official benchmark mode ([#32](https://github.com/wiggzz/carry/issues/32)) ([99e3a69](https://github.com/wiggzz/carry/commit/99e3a69d944ef631d3570487400ac37d9c2b2f0e))


### Bug Fixes

* aggregate response retry metrics ([a509f23](https://github.com/wiggzz/carry/commit/a509f23d0b2a04205ab42210637746edf5d3506d))
* authorize canonical launch template resource ([#24](https://github.com/wiggzz/carry/issues/24)) ([4fe9644](https://github.com/wiggzz/carry/commit/4fe9644e3505ffcaa591535a0befaf0677dd6ba7))
* authorize launch-template network resources ([#25](https://github.com/wiggzz/carry/issues/25)) ([2d64829](https://github.com/wiggzz/carry/commit/2d64829131cc2609f63872968f380ea185e8538e))
* authorize launches by launch template ARN ([#22](https://github.com/wiggzz/carry/issues/22)) ([399e966](https://github.com/wiggzz/carry/commit/399e966db63e28494ce3cc4a9d4311a19173c1e2))
* honor bounded rate-limit delays ([e1a42fa](https://github.com/wiggzz/carry/commit/e1a42fa0d097823469c151953249a25ba33ce089))
* honor bounded rate-limit delays ([#31](https://github.com/wiggzz/carry/issues/31)) ([9b97370](https://github.com/wiggzz/carry/commit/9b973709f8eb2e635b748c2ce540f20774209944))
* retry transient Responses API failures ([9b97370](https://github.com/wiggzz/carry/commit/9b973709f8eb2e635b748c2ce540f20774209944))
* retry transient Responses API failures ([dd1f284](https://github.com/wiggzz/carry/commit/dd1f2846d48d79dd81c9731b1675a4b1e4e3c45d))
* use supported EC2 launch count option ([#20](https://github.com/wiggzz/carry/issues/20)) ([0d6d113](https://github.com/wiggzz/carry/commit/0d6d113b0ed8a1f1066a7096ca95e720682a0d2f))
* wait through pending worker startup ([#26](https://github.com/wiggzz/carry/issues/26)) ([2fe7539](https://github.com/wiggzz/carry/commit/2fe75399c0c840d37489cdc8d1c255b21c0c17d1))

## [0.2.1](https://github.com/wiggzz/carry/compare/v0.2.0...v0.2.1) (2026-08-16)


### Bug Fixes

* trust GitHub immutable OIDC subjects ([#18](https://github.com/wiggzz/carry/issues/18)) ([f174bac](https://github.com/wiggzz/carry/commit/f174bac18ff192c95dfe159de59002b442c78c41))

## [0.2.0](https://github.com/wiggzz/carry/compare/v0.1.1...v0.2.0) (2026-08-16)


### Features

* add protected EC2 benchmark bootstrap ([#7](https://github.com/wiggzz/carry/issues/7)) ([#15](https://github.com/wiggzz/carry/issues/15)) ([3983db5](https://github.com/wiggzz/carry/commit/3983db59f09216eb9aefd34e7a7ca8cefffc3bb5))

## [0.1.1](https://github.com/wiggzz/carry/compare/v0.1.0...v0.1.1) (2026-08-16)


### Bug Fixes

* upgrade release please to Node 24 ([#9](https://github.com/wiggzz/carry/issues/9)) ([7bd9e3b](https://github.com/wiggzz/carry/commit/7bd9e3b313550d3e6b8edaf80b0d137f86ad9edf))

## 0.1.0 (2026-08-16)


### Features

* add ephemeral benchmark worker infrastructure ([#6](https://github.com/wiggzz/carry/issues/6)) ([09c41d4](https://github.com/wiggzz/carry/commit/09c41d4f5d1448e19728fefb60b4f133ed0324cf))
