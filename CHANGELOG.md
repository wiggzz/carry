# Changelog

## [0.12.1](https://github.com/wiggzz/carry/compare/v0.12.0...v0.12.1) (2026-10-18)


### Bug Fixes

* **proxy:** bound traces and preserve safe operational diagnostics ([#154](https://github.com/wiggzz/carry/issues/154)) ([279dc3b](https://github.com/wiggzz/carry/commit/279dc3be9753902972f68b09b2d67030dc29412b))
* **proxy:** preserve successful-request dashboard savings ([#157](https://github.com/wiggzz/carry/issues/157)) ([65c177e](https://github.com/wiggzz/carry/commit/65c177e52600f30d177f953d65c5b94a9af1507d))

## [0.12.0](https://github.com/wiggzz/carry/compare/v0.11.0...v0.12.0) (2026-10-17)


### Features

* add cost report using shared savings estimator ([f614abf](https://github.com/wiggzz/carry/commit/f614abf4c3dae5c0d231bd406ee2bb2c89d8daeb))
* add qualified local session cost and savings reports ([#139](https://github.com/wiggzz/carry/issues/139)) ([a47fab7](https://github.com/wiggzz/carry/commit/a47fab7bc3f8be3d87fbc481e53bface43d69c0b))
* **bench:** add isolated native-client proxy lanes and complete usage ledgers ([1f83d1c](https://github.com/wiggzz/carry/commit/1f83d1c6609fd5690c2412a04b7f96b2ba9e2219))
* estimate Pi-style compaction savings in web UI ([76d2998](https://github.com/wiggzz/carry/commit/76d29987ec9575fe1e6acd2735e971b56d0c3e36))
* open cost report from persistent temp file ([0101276](https://github.com/wiggzz/carry/commit/0101276851ba94aa43cbf9d02199a8baaee4cb26))
* **proxy:** add native-client context retention and local session support ([#141](https://github.com/wiggzz/carry/issues/141)) ([570668c](https://github.com/wiggzz/carry/commit/570668cb5b03171b1559f5174143ab18892d32d5))
* **proxy:** add opaque Responses HTTP frontdoor ([be9a45f](https://github.com/wiggzz/carry/commit/be9a45fd3c16665cbe9bdb384f91855df8872891))
* **proxy:** bound reviewer windows and estimate session savings ([311e63a](https://github.com/wiggzz/carry/commit/311e63acc64ea3f2be64738f47f178523111f233))
* **proxy:** expose numeric attempt accounting and censor transport failures ([df9e2d6](https://github.com/wiggzz/carry/commit/df9e2d6e5c684c1ac42c2d01f9c382c4c65eaa63))
* **proxy:** integrate shared retention economics and paired reviewer projection ([23529b7](https://github.com/wiggzz/carry/commit/23529b75924cc96e8c412817c8197c184846567b))
* **proxy:** replay native review context and retire obsolete memories ([2887dcc](https://github.com/wiggzz/carry/commit/2887dcc5620807e870652c05a34110970ebd0f49))
* **proxy:** reuse active prefixes and bootstrap bounded history ([1ebc409](https://github.com/wiggzz/carry/commit/1ebc409dddbebc0815b4e4d535158f645320195c))
* **proxy:** run ordinary reviews from background snapshots ([8731e28](https://github.com/wiggzz/carry/commit/8731e28ce5116ca39de622451b40fdb4b22defbf))
* **proxy:** share reviewer history and plan context capacity ([#144](https://github.com/wiggzz/carry/issues/144)) ([444e55e](https://github.com/wiggzz/carry/commit/444e55e4ffe442d94ebc94b08d1e6d3d829734b4))
* **proxy:** support local Pi sessions and subscription monitoring ([ff16389](https://github.com/wiggzz/carry/commit/ff16389ea4e565cfa21cb3422bde0af600362bde))
* **proxy:** support standalone startup and replay history review ([ebb07df](https://github.com/wiggzz/carry/commit/ebb07df0eab7ed50cf8332df2d6c2be3b4dfdbd5))
* show aggregate cost and savings cards in report ([95e4704](https://github.com/wiggzz/carry/commit/95e470449a6dce1dd3a543cec2d94491cac17384))
* show estimated savings percentage in UI and report ([8b09e9f](https://github.com/wiggzz/carry/commit/8b09e9f7a4d69b79ef756dc81a3c33e2322f3bf3))


### Bug Fixes

* **bench:** block provider-hosted retrieval before model dispatch ([e8ed3d6](https://github.com/wiggzz/carry/commit/e8ed3d6599473ebb23d88498aa95c48d9abacc73))
* **bench:** keep censored task-attempt cost totals unavailable ([78328f0](https://github.com/wiggzz/carry/commit/78328f084f34808d509adceca44940dfe10b43ce))
* **bench:** keep missing cache and unpriced output usage cost unavailable ([2916f44](https://github.com/wiggzz/carry/commit/2916f44edfd85da7dc5804de02b57a491ed37dc7))
* **benchmark:** harden cleanup and native usage evidence ([565f5b8](https://github.com/wiggzz/carry/commit/565f5b8bd25617a527fb2658197d26b7482b8dd3))
* **benchmark:** trust operator checkpoint policy and bound gateway telemetry ([4dc432b](https://github.com/wiggzz/carry/commit/4dc432b388ed7da466e624f8870b6ea4632f8282))
* **bench:** recognize exact Docker network absence during proxy cleanup ([d0c919b](https://github.com/wiggzz/carry/commit/d0c919b7f6fd63b4afe8fc63f74038e4b0dd8ce7))
* **ci:** preserve off as a literal proxy dispatch choice ([125b19f](https://github.com/wiggzz/carry/commit/125b19f88cb4e69ce59e9828ba6c32c6df09731c))
* **ci:** retain pinned Node in isolated Codex fixture path ([1fc6de9](https://github.com/wiggzz/carry/commit/1fc6de94797c13db7e7bb3f1f92e9f53bcfa88cd))
* **core:** censor overflowing native usage aggregates ([5e88100](https://github.com/wiggzz/carry/commit/5e88100506b83441098cc7b9d6100fb19ab130b2))
* **core:** mark unsupported billed audio usage cost unavailable ([cb2a549](https://github.com/wiggzz/carry/commit/cb2a5497d369373fffb31de7ea679f1bcb801d2b))
* lower default payback percentage to 10 ([#145](https://github.com/wiggzz/carry/issues/145)) ([3886b8f](https://github.com/wiggzz/carry/commit/3886b8f9a953ff269a297f54df62123e5e0ad6a5))
* **models:** centralize pricing and cache capabilities ([2ad3b40](https://github.com/wiggzz/carry/commit/2ad3b4031b27a9165ebf27ee7b124234ce33b7f4)), closes [#143](https://github.com/wiggzz/carry/issues/143)
* preserve migrated context when resuming checkpoints ([#147](https://github.com/wiggzz/carry/issues/147)) ([53bfa72](https://github.com/wiggzz/carry/commit/53bfa72a83faffbcee8cc8d7500f4f4c8ac06eaf))
* **proxy:** accept valid review items and return rejection feedback ([4d688ad](https://github.com/wiggzz/carry/commit/4d688ad2dcc1271b7a503e28b79dc890fa349e31))
* **proxy:** anchor restart lineage to native compaction checkpoints ([fc1a298](https://github.com/wiggzz/carry/commit/fc1a298a9f8b259ada793a0ca54389028c036281))
* **proxy:** bind drop advice to reviewed native membership ([f806cd3](https://github.com/wiggzz/carry/commit/f806cd3c0d261ecb166992c615aebb44ef9310f5))
* **proxy:** bound classifier cache affinity and diagnose rejections ([b37c373](https://github.com/wiggzz/carry/commit/b37c3735495d362c877d3d8823e76721ccc4dea9))
* **proxy:** bound shared memory by rendered reviewer budgets ([40bdb52](https://github.com/wiggzz/carry/commit/40bdb525ae49948a00c03159f77812162c279d9a))
* **proxy:** clarify dashboard budgets and session roles ([#146](https://github.com/wiggzz/carry/issues/146)) ([779cc4e](https://github.com/wiggzz/carry/commit/779cc4e84b7a1c4be580c44fb50629c52740801b))
* **proxy:** construct reviewer prefix as JSON macro expression ([9d4ec92](https://github.com/wiggzz/carry/commit/9d4ec92f24f989240e827a428337a1049311223c))
* **proxy:** distinguish partially accepted reviews ([44b3dac](https://github.com/wiggzz/carry/commit/44b3dace72df724b3ffa6477bd50d742357f0dd2))
* **proxy:** exclude incomplete sessions from dashboard estimates ([2adcf7c](https://github.com/wiggzz/carry/commit/2adcf7ce61b6b6d92868b9f227f9e36db8ee94c1))
* **proxy:** expand continuations from successful response frontier ([ae5f8c4](https://github.com/wiggzz/carry/commit/ae5f8c4c03d93d43a2d943cd176dd40bea812aff))
* **proxy:** fail closed on unpriced pending reviewer work ([27032e5](https://github.com/wiggzz/carry/commit/27032e5f9578c68dd33a66cde0579c3d31946e05))
* **proxy:** keep custom tool review steps atomic ([8723396](https://github.com/wiggzz/carry/commit/8723396965a3b69702fedb4929013b472bcb699c))
* **proxy:** observe immutable members once and ground facts in main sources ([573786d](https://github.com/wiggzz/carry/commit/573786dbd5c516fdcefff435e9e454d8bab3b778))
* **proxy:** parse final review advice and scope background guards ([e131a2a](https://github.com/wiggzz/carry/commit/e131a2ad7263adeac81af9c892f4b5a90287ccb6))
* **proxy:** pin securely known opaque native completion cohorts ([9d4ccfc](https://github.com/wiggzz/carry/commit/9d4ccfc5e409f214a03b7566a134b56fa91cd70c))
* **proxy:** preserve cache evidence across economic review windows ([b7ab759](https://github.com/wiggzz/carry/commit/b7ab7590d482982b1a2c8359201de278e51cfac5))
* **proxy:** preserve current native wire metadata on tolerant echoes ([60ec2b4](https://github.com/wiggzz/carry/commit/60ec2b4c98e957173b9854afb85411ff165c521b))
* **proxy:** preserve native schemas and bound SSE observation ([4046480](https://github.com/wiggzz/carry/commit/4046480145d3c4f1ac4c90b1fb4b01aa589bbc27))
* **proxy:** preserve retained native compaction and failed SSE accounting ([632ce84](https://github.com/wiggzz/carry/commit/632ce84d7445c5d9354ee06c5eca45767c41c8d5))
* **proxy:** preserve reviewer cache prefixes across turns and windows ([17503d5](https://github.com/wiggzz/carry/commit/17503d5ec0a6a796718e2e0e901b2e74087f49d4))
* **proxy:** price complete review sweeps on the actual wire ([fe7439e](https://github.com/wiggzz/carry/commit/fe7439e135661c5e94cadc5ba9bcc32ae5917b4e))
* **proxy:** price future reviewer outputs in paired payback margin ([83d04a8](https://github.com/wiggzz/carry/commit/83d04a8f342a3437af2e9619ccb55def3aedcdfe))
* **proxy:** put JSON-mode instruction in classifier input ([2f8e48c](https://github.com/wiggzz/carry/commit/2f8e48ccf1d3cb3343a3405d53578347217af2a0))
* **proxy:** rebase explicit native replacements and recognize streamed checkpoints ([df4a4d6](https://github.com/wiggzz/carry/commit/df4a4d6cac0c8f8f3b9fc619957532e276062b0e))
* **proxy:** recover streamed review output from completed items ([d84f39c](https://github.com/wiggzz/carry/commit/d84f39c6d39f9a7fca8d3479b857aacc33927512))
* **proxy:** require native anchors and report kernel-bound listeners ([d090c45](https://github.com/wiggzz/carry/commit/d090c45e1d2faf64b55e2a67dd4e9052d6253d3c))
* **proxy:** retain safe provider stream error diagnostics ([a2b3c75](https://github.com/wiggzz/carry/commit/a2b3c75ceacf83633af53f317d0e4adfd4ab9fb7))
* **proxy:** use map entry API for reviewer opinions ([3d4d2ea](https://github.com/wiggzz/carry/commit/3d4d2eaa3acd265005a9bb76374f708786eaeb5a))
* **proxy:** write stable reviewer cache boundaries and bound checkpoint evidence ([e729f5b](https://github.com/wiggzz/carry/commit/e729f5b17fe323f7bbcf121096da63d0066f34b0))
* **report:** separate modeled usage from conditional savings scenarios ([5d54b65](https://github.com/wiggzz/carry/commit/5d54b6592e2a0524002b5c95e9d46f2bf5ced2f8))
* **savings:** price frozen output at each branch input tier ([af9b54c](https://github.com/wiggzz/carry/commit/af9b54c302db02fe50b5605b71b867fc2c149038))

## [0.11.0](https://github.com/wiggzz/carry/compare/v0.10.0...v0.11.0) (2026-09-28)


### Features

* **defaults:** use Sol for Carry and Luna for SWE-bench ([#130](https://github.com/wiggzz/carry/issues/130)) ([97c2b49](https://github.com/wiggzz/carry/commit/97c2b49b988c605e609021e67332c3ab38508b1e))
* improve context retention and session visibility ([#136](https://github.com/wiggzz/carry/issues/136)) ([05a3f4c](https://github.com/wiggzz/carry/commit/05a3f4cf8df39543c48a83db2bf373633342486e))
* **shell:** let model set command timeouts ([#133](https://github.com/wiggzz/carry/issues/133)) ([3649cf4](https://github.com/wiggzz/carry/commit/3649cf4c019ae9d0ad7287708eb14367d251daaa))
* **web:** show reasoning and token usage in footer ([4de7c59](https://github.com/wiggzz/carry/commit/4de7c5911814b109360ae75cf2f78e0eba285d1b))
* **web:** show remembered notes in activity ([a3a9f16](https://github.com/wiggzz/carry/commit/a3a9f16505f9f3b7b772168f065c956da5cb1356))
* **web:** show selected model in footer ([003a129](https://github.com/wiggzz/carry/commit/003a129b1b71160ffdcea7e204100bb65b316adf))


### Bug Fixes

* **benchmark:** fail closed on duplicate SymPy tests ([#127](https://github.com/wiggzz/carry/issues/127)) ([64b8f00](https://github.com/wiggzz/carry/commit/64b8f0095ceb7a65f823bc7c6c8ad7fc788ce6c5))
* **benchmark:** grade captured patches after task timeouts ([#131](https://github.com/wiggzz/carry/issues/131)) ([dccddf0](https://github.com/wiggzz/carry/commit/dccddf081e2975137adb7bae0a11a5aea789fa96))
* **benchmark:** repair standard catalog readiness recipes ([#129](https://github.com/wiggzz/carry/issues/129)) ([4febda6](https://github.com/wiggzz/carry/commit/4febda680d0dbfc7d4245c882cd59dfb7e689197))
* **context:** preserve guidance checks and tool failure learnings ([a3808c1](https://github.com/wiggzz/carry/commit/a3808c146bc6e9b4f111b7a5703042cd3baf7ed5))
* **context:** protect remembered facts by default ([71d135e](https://github.com/wiggzz/carry/commit/71d135e1d57c5620473e7e72dea69e733ab82230))
* focus web input and keep web logs off terminal ([7fed255](https://github.com/wiggzz/carry/commit/7fed2558fbe8d0e79aaa8c48abab5be020196dc3))
* **prompt:** omit MCP instructions without configured servers ([e57d47b](https://github.com/wiggzz/carry/commit/e57d47b962073a4895b4e97c790063ef3ff994d4))
* retain assistant text preceding tool calls ([91e73d2](https://github.com/wiggzz/carry/commit/91e73d23863fb96015b3630c430053b3f6cfc270))
* **test:** avoid redundant closure in web session test ([be5b289](https://github.com/wiggzz/carry/commit/be5b289b565988d55bf324b86239194cbf954249))

## [0.10.0](https://github.com/wiggzz/carry/compare/v0.9.0...v0.10.0) (2026-09-23)


### Features

* add MCP CLI support ([#95](https://github.com/wiggzz/carry/issues/95)) ([6f10962](https://github.com/wiggzz/carry/commit/6f10962db50b6b794db6ce6d3be2a097a6086629))
* **compaction:** add paired rollout policy ([#114](https://github.com/wiggzz/carry/issues/114)) ([ebb4f0a](https://github.com/wiggzz/carry/commit/ebb4f0ad2b413bc16e582679df4be3f689bf3675))
* **context:** add payoff margin and long-cohort validation ([#119](https://github.com/wiggzz/carry/issues/119)) ([0fe9a9f](https://github.com/wiggzz/carry/commit/0fe9a9f9753ef6f9828676c3640599816d1cefe5))
* **context:** gate keep-lease review by planner ([#116](https://github.com/wiggzz/carry/issues/116)) ([3211310](https://github.com/wiggzz/carry/commit/32113105f11e49627acb7307b3b8e2111733066b))
* improve web and terminal session experience ([#120](https://github.com/wiggzz/carry/issues/120)) ([655413c](https://github.com/wiggzz/carry/commit/655413c36913e8c9fa996ec38cb52dc4dd56078f))


### Bug Fixes

* **benchmark:** account timed-out Carry usage ([#117](https://github.com/wiggzz/carry/issues/117)) ([af6d913](https://github.com/wiggzz/carry/commit/af6d9136b74a42734b3490fa5c74fa5acf22d88b))
* **context:** preserve cache prefix during keep-lease review ([#118](https://github.com/wiggzz/carry/issues/118)) ([419c741](https://github.com/wiggzz/carry/commit/419c741dfa3463a1ab026e4f5ec21e6b577d23ce))
* refresh invalidated ChatGPT subscription tokens ([#122](https://github.com/wiggzz/carry/issues/122)) ([0ba814c](https://github.com/wiggzz/carry/commit/0ba814cef6ff8d91b6b9ca0ed51f029ec1b5c0b3))

## [0.9.0](https://github.com/wiggzz/carry/compare/v0.8.2...v0.9.0) (2026-09-15)


### Features

* **bench:** parameterize SWE-bench official-50 attempts ([#113](https://github.com/wiggzz/carry/issues/113)) ([a46e5e5](https://github.com/wiggzz/carry/commit/a46e5e5fe364d768d24ab44b8478a889ed30deb8))


### Bug Fixes

* preserve cache affinity across follow-ups ([#111](https://github.com/wiggzz/carry/issues/111)) ([539bce5](https://github.com/wiggzz/carry/commit/539bce5cbb0a3febf66feb7028adccd08c5fff14))

## [0.8.2](https://github.com/wiggzz/carry/compare/v0.8.1...v0.8.2) (2026-09-14)


### Bug Fixes

* add copy-only Regex evidence recovery ([#107](https://github.com/wiggzz/carry/issues/107)) ([22a2327](https://github.com/wiggzz/carry/commit/22a232792e24a6e9da08b858acd79d0e147433e5))
* harden FrontierHarness checkpoint and evidence pipeline ([#101](https://github.com/wiggzz/carry/issues/101)) ([d2a50af](https://github.com/wiggzz/carry/commit/d2a50af09242543c481ba1f2922fc49df3abf0b6))

## [0.8.1](https://github.com/wiggzz/carry/compare/v0.8.0...v0.8.1) (2026-09-13)


### Bug Fixes

* account for implicit OpenAI cache writes ([#94](https://github.com/wiggzz/carry/issues/94)) ([ffdf024](https://github.com/wiggzz/carry/commit/ffdf0249053987496babe97a4f90310f3a640a35))
* allow legitimate repository actions ([#92](https://github.com/wiggzz/carry/issues/92)) ([330fe3b](https://github.com/wiggzz/carry/commit/330fe3bb59882e38916ac8bed3e51bdeab5b8252))
* amortize compaction over five requests ([#104](https://github.com/wiggzz/carry/issues/104)) ([af803f7](https://github.com/wiggzz/carry/commit/af803f7632ba61b3f9e259d6a03014b8d05ea1ba))
* preserve agent changes after git commits ([#102](https://github.com/wiggzz/carry/issues/102)) ([ca22c9d](https://github.com/wiggzz/carry/commit/ca22c9dda0e7a7b6550b19865c22c3405f57625b))
* preserve human intent during compaction ([#99](https://github.com/wiggzz/carry/issues/99)) ([e7dc188](https://github.com/wiggzz/carry/commit/e7dc188c614d366c21fcd65b5a668d0c4595b4e4))
* preserve raw stdout and stderr payloads ([#103](https://github.com/wiggzz/carry/issues/103)) ([366dc7d](https://github.com/wiggzz/carry/commit/366dc7dd1fccbdcc80f14ae0c882ccbcb04289f8))
* prioritize critical context retention ([#105](https://github.com/wiggzz/carry/issues/105)) ([9d1ed13](https://github.com/wiggzz/carry/commit/9d1ed13c2c20142f72702d54ff42cd332a4dfa93))

## [0.8.0](https://github.com/wiggzz/carry/compare/v0.7.1...v0.8.0) (2026-09-11)


### Features

* support ChatGPT Codex subscriptions ([#86](https://github.com/wiggzz/carry/issues/86)) ([8fa6a5a](https://github.com/wiggzz/carry/commit/8fa6a5a52efe68fcfe3c90c34d5f1bb4a8b68026))


### Bug Fixes

* avoid mapfile in subnet migration ([#84](https://github.com/wiggzz/carry/issues/84)) ([35b39ca](https://github.com/wiggzz/carry/commit/35b39caa3e3fddb3b28c0ce655dc298564e65769))
* bound benchmark worker user data ([#82](https://github.com/wiggzz/carry/issues/82)) ([a631f11](https://github.com/wiggzz/carry/commit/a631f11242238033481f5a898160affa32157e97))
* centralize benchmark agent commands ([#80](https://github.com/wiggzz/carry/issues/80)) ([3ea9555](https://github.com/wiggzz/carry/commit/3ea955596a835d10dc15e5f231efdee14018a645))
* deduplicate replayed SSE events ([#91](https://github.com/wiggzz/carry/issues/91)) ([acb9e88](https://github.com/wiggzz/carry/commit/acb9e88625e03c0d0534d5b226072277c5bc94a3))
* derive multi-AZ benchmark dispatch configuration ([#58](https://github.com/wiggzz/carry/issues/58)) ([c6a55ee](https://github.com/wiggzz/carry/commit/c6a55eee50e19443b1cfc166ff3ae557a4fee454))
* make terraform apply script first-run portable ([#85](https://github.com/wiggzz/carry/issues/85)) ([70fe60b](https://github.com/wiggzz/carry/commit/70fe60b50a7dd03967f0a5b2159316e097ab2dfc))
* migrate legacy worker subnet config ([#83](https://github.com/wiggzz/carry/issues/83)) ([00bc04c](https://github.com/wiggzz/carry/commit/00bc04c9d73ee013ed1466b27e1087d530370fee))
* preserve timed-out shell diagnostics ([#90](https://github.com/wiggzz/carry/issues/90)) ([0f8ec28](https://github.com/wiggzz/carry/commit/0f8ec28767140b15995dfb9f0a8ed1c9dd0027bb))

## [0.7.1](https://github.com/wiggzz/carry/compare/v0.7.0...v0.7.1) (2026-09-04)


### Bug Fixes

* allow nonbreaking Conventional Commit titles ([#78](https://github.com/wiggzz/carry/issues/78)) ([3973e3e](https://github.com/wiggzz/carry/commit/3973e3ec4581601e245a1039139a7d121a55ce32))
* document recent context benchmark evidence ([#79](https://github.com/wiggzz/carry/issues/79)) ([c0451cf](https://github.com/wiggzz/carry/commit/c0451cf284d4fd9f55cb7bbbd7b3a2ad467b08d2))

## [0.7.0](https://github.com/wiggzz/carry/compare/v0.6.0...v0.7.0) (2026-09-04)

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
