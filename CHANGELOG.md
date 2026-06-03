# Changelog

All notable changes to this project are documented here.
Format based on [Keep a Changelog](https://keepachangelog.com/),
versioning follows [SemVer](https://semver.org/).
## [0.3.3] - 2026-06-03

### Documentation

- Update we're already doing delta with datafusion backend too ([011ec82](https://github.com/jeroenflvr/fast-api/commit/011ec8225b24a8dde0a2795d187d4b424d1ab15b))

### Refactor

- More perf improvements (cow) ([0a8d083](https://github.com/jeroenflvr/fast-api/commit/0a8d083478b57b53e6b571b471b2f0689fb990d5))

### Bench

- Optimize vectorized operations on json data ([28d9908](https://github.com/jeroenflvr/fast-api/commit/28d990821d1862dfc43c04d5a121359642f274d2))

## [0.3.2] - 2026-06-03

### Bug Fixes

- Lazy loading for duckdb backend ([0fa5da5](https://github.com/jeroenflvr/fast-api/commit/0fa5da503d806f55d7c42fb86332f417fc7a3589))

## [0.3.1] - 2026-06-03

### Bug Fixes

- *(ci)* Include explorer feature in every build ([c5db03f](https://github.com/jeroenflvr/fast-api/commit/c5db03f5d7743f960b9b6111c20b53c9c807ae31))

## [0.3.0] - 2026-06-02

### Documentation

- Update name ([5816d4b](https://github.com/jeroenflvr/fast-api/commit/5816d4b2eda0534e0a30b8c1a44a848a709e7330))

### Features

- *(explorer)* Add a duckdb wasm terminal ([a8de1e6](https://github.com/jeroenflvr/fast-api/commit/a8de1e66148f1ffee6c7e45dc62593308d1cbb67))
- Add embedded explorer website with tables etc, and duckdb wasm terminal ([9469336](https://github.com/jeroenflvr/fast-api/commit/9469336db8568f9cd4a6b837002538cfd65903bb))

## [0.2.18] - 2026-06-02

### Bug Fixes

- *(ci)* Cargo clippy warnings ([b8342c7](https://github.com/jeroenflvr/fast-api/commit/b8342c7b58468a0c1216305588dc43869d1bbedb))

### Documentation

- Add duckdb-wasm instance to mkdocs ([45c1f61](https://github.com/jeroenflvr/fast-api/commit/45c1f61fa3ca2c7d642e64116b2b8fe3f7e438db))

### Features

- Add streaming parquet endpoints for duckdb client ([4d15a64](https://github.com/jeroenflvr/fast-api/commit/4d15a647339a8bfe2405345f7e57011088aabb96))
- Align s3 config between datafusion and duckdb ([36b915d](https://github.com/jeroenflvr/fast-api/commit/36b915db9e4c185aa6ab3b39b8ca65d994ecc8aa))

## [0.2.17] - 2026-06-01

### Features

- Pass in callable for hmac secrets fetching with python ([8757887](https://github.com/jeroenflvr/fast-api/commit/8757887741e746b071746e68fba199031304754a))

### Styling

- Better version badge in mkdocs (css) ([345ea2d](https://github.com/jeroenflvr/fast-api/commit/345ea2d174c1844443ad208fa9a10926710db01d))

## [0.2.16] - 2026-05-31

### Documentation

- Update readme and doc links ([8fd6701](https://github.com/jeroenflvr/fast-api/commit/8fd67013b791cd32032cfb372970e5812abc8aee))

### Features

- Add option to generate dataseets.toml template from datapress ([878f1a2](https://github.com/jeroenflvr/fast-api/commit/878f1a2b60ba6b578b20a0c147ce60f2d243112d))

## [0.2.15] - 2026-05-31

### Features

- Add cli tool, installable binary ([6ac8538](https://github.com/jeroenflvr/fast-api/commit/6ac8538ffcb189ff63a9d9acff1f7cf8fc0eb386))

## [0.2.14] - 2026-05-31

### Bug Fixes

- Oidc on swagger ui ([6a8c0bc](https://github.com/jeroenflvr/fast-api/commit/6a8c0bc0daa8d968d72019a7e864e04a854853df))
- *(sec)* Fix sec bugs ([9cc09f3](https://github.com/jeroenflvr/fast-api/commit/9cc09f32cbe4f9ba443770eb3249743f74fd2244))

## [0.2.13] - 2026-05-31

### Bug Fixes

- Swagger auth ([0307c7a](https://github.com/jeroenflvr/fast-api/commit/0307c7a4070a499fd08750d3045a732eb894655d))

## [0.2.12] - 2026-05-31

### Features

- Add auth to swagger ui from python ([d5a3ada](https://github.com/jeroenflvr/fast-api/commit/d5a3adaf343cf09e2a242e0040fcc8c66233bc5f))

## [0.2.11] - 2026-05-31

### Bug Fixes

- *(oidc)* Make sub claim optional ([d0fc43d](https://github.com/jeroenflvr/fast-api/commit/d0fc43d9d46965d45b9555f72749e8e9c5aba3cc))

## [0.2.10] - 2026-05-31

### Bug Fixes

- *(oidc)* Aud and scope are allowed to return both arrays and strings ([53074e4](https://github.com/jeroenflvr/fast-api/commit/53074e48345d8cd442518a6078d375101fd24070))

## [0.2.9] - 2026-05-31

### Documentation

- Update links (pypi) ([31c81b5](https://github.com/jeroenflvr/fast-api/commit/31c81b50fadb0ccf1be4906863cedeccc92feb4d))
- Update entry page ([aee0a87](https://github.com/jeroenflvr/fast-api/commit/aee0a87f68688073558bedca9ab58418d28c1bfd))

### Miscellaneous

- *(doc)* Fmt ([ea21b58](https://github.com/jeroenflvr/fast-api/commit/ea21b5809f2a5934db45c8e6ee660f980b31855c))
- *(doc)* Fmt ([438ba1c](https://github.com/jeroenflvr/fast-api/commit/438ba1c54cb4bc87f2c351ef5b20af88e8a07afe))

## [0.2.8] - 2026-05-31

### Bug Fixes

- Oidc crypto provider ([5a61131](https://github.com/jeroenflvr/fast-api/commit/5a611318e2be1be20f3ebbf7bf792fa3c47c2738))

### CI

- Publish mkdocs site ([29e0bb8](https://github.com/jeroenflvr/fast-api/commit/29e0bb893db8c982f3c23975aecde4b46dc62e32))

### Documentation

- Update for oidc integration ([acfcdff](https://github.com/jeroenflvr/fast-api/commit/acfcdff0f66252bc836164d6dc1dfee7209a9aab))

## [0.2.7] - 2026-05-31

### Bug Fixes

- S3 secret config for duckdb ([8b0dd3f](https://github.com/jeroenflvr/fast-api/commit/8b0dd3fdd6f94a5c65713e41f5a36da5c517518c))

### CI

- Add docs and mkdocs to develop build ([79db0d4](https://github.com/jeroenflvr/fast-api/commit/79db0d40f70275e7c2a3f9416172d6438387a4cd))

## [0.2.6] - 2026-05-30

### Bug Fixes

- Clippy ([299a67d](https://github.com/jeroenflvr/fast-api/commit/299a67d060794c652555b7b5b66d8d49a6f63f8f))

### Features

- Implement quach protocol for duckdb (seriously) ([746675e](https://github.com/jeroenflvr/fast-api/commit/746675ebdef0b72a638b3689acc192a1ac292407))
- When duckdb, allow new quack for direct connection ([fc8f845](https://github.com/jeroenflvr/fast-api/commit/fc8f845171cae66acabb706792794e7853fb2002))

### Miscellaneous

- *(deps)* Bump the rust-dependencies group across 1 directory with 3 updates ([df3ed0e](https://github.com/jeroenflvr/fast-api/commit/df3ed0ec85837d33efd1b0f27045031dc74f77f6))
- *(deps)* Bump the github-actions group across 1 directory with 5 updates ([21c685f](https://github.com/jeroenflvr/fast-api/commit/21c685fc5276ea0d62eabaa8bb447427d392871b))

## [0.2.5] - 2026-05-30

### Documentation

- Updating arrow ipc ([6a21946](https://github.com/jeroenflvr/fast-api/commit/6a2194672a548e981254be25113d6145a64f864a))
- Clarify different compression handlers: gzip/deflate mostly handled automatically, brotli needs additional handling ([5053a4d](https://github.com/jeroenflvr/fast-api/commit/5053a4d631d17ff5ad88735c7c31dfe835877573))
- Clarify request and response config ([ac9aa30](https://github.com/jeroenflvr/fast-api/commit/ac9aa30f5c60fb3ac372dc709f026306218418b9))
- Update mkdocs and add logo ([b6661ca](https://github.com/jeroenflvr/fast-api/commit/b6661ca5b91bcd4cb862599d89350d1f749b5763))
- Update on double buffer handling ([5aa28c8](https://github.com/jeroenflvr/fast-api/commit/5aa28c8141e837a8f644c991e734ff965bbaabfe))

### Features

- Update to full stream support (first step) ([58a6b47](https://github.com/jeroenflvr/fast-api/commit/58a6b479dd274614d546125bbf3c9d1840f50a81))

### Miscellaneous

- Rust fmt ([9eddee4](https://github.com/jeroenflvr/fast-api/commit/9eddee4bd0f0392969b814a49b9610fa34c84f52))

## [0.2.4] - 2026-05-30

### Miscellaneous

- Always build with metrics ([ec1c7ee](https://github.com/jeroenflvr/fast-api/commit/ec1c7eec76e21333eb504f60803b6b59a12d85b7))
- Pin mkdocs version to avoid 2.0 breaking changes ([d17e509](https://github.com/jeroenflvr/fast-api/commit/d17e50952d1cf0cdec3b95b4d160297030309adb))
- Cleanup projects and align crates ([74f6d6d](https://github.com/jeroenflvr/fast-api/commit/74f6d6d2443a759f914747952f9fe2dbb623c9d2))
- Update community standards ([9d35495](https://github.com/jeroenflvr/fast-api/commit/9d354955609512c74d5f7078ca73ec010f050eb7))

## [0.2.3] - 2026-05-29

### Bug Fixes

- Group_by on duckdb backend ([b7cfd0f](https://github.com/jeroenflvr/fast-api/commit/b7cfd0f49ae441ef3cea31e6bd0a84f910042c51))

### CI

- Add dependency audit ([99665a4](https://github.com/jeroenflvr/fast-api/commit/99665a4149119042fa99fec2c6fc00c01d121ef5))

### Documentation

- Update python bindings doc ([b811f6a](https://github.com/jeroenflvr/fast-api/commit/b811f6a6ed53cd79d11bb872ac2c0623aaf9a298))

### Features

- Raising community standards ([d5010db](https://github.com/jeroenflvr/fast-api/commit/d5010db7e334332fcdf87f8b81a927b02c83678b))
- Add prometheus exports ([3335c93](https://github.com/jeroenflvr/fast-api/commit/3335c93d598a950c71089f89f40041c7a196fa2f))
- Support hive-style partitioning of parquet data ([c171b23](https://github.com/jeroenflvr/fast-api/commit/c171b23e1bf684b66e95fa8c1aec0e3a29ce5a97))

## [0.2.2] - 2026-05-28

### Bug Fixes

- *(ci)* Don't trigger on main ([80b54ef](https://github.com/jeroenflvr/fast-api/commit/80b54ef39ac6c6e79558afb6a5bb4e568b036b90))

## [0.2.1] - 2026-05-28

### Bug Fixes

- *(tests)* Done ([04e9ff2](https://github.com/jeroenflvr/fast-api/commit/04e9ff2b5084df11d5c35665aab5fc45517d7ba4))

## [0.2.0] - 2026-05-28

### Documentation

- Update auth doc ([9f83459](https://github.com/jeroenflvr/fast-api/commit/9f83459e051b4cd2e061644b8ff0b2a3d188cbf7))

### Features

- Add pluggable auth ([510592f](https://github.com/jeroenflvr/fast-api/commit/510592f3c6753a57507ae8ea2741378ff0db7f4c))
- Add docs/mkdocs/version/.. ([5e2cb04](https://github.com/jeroenflvr/fast-api/commit/5e2cb0406ecacf0df6123386aa27ae89edd12280))

## [0.1.18] - 2026-05-27

### Documentation

- Add mkdocs, embedded ([8d3ef25](https://github.com/jeroenflvr/fast-api/commit/8d3ef258f44e3b1a6ca129d72fd06553f7a840d7))

### Features

- Add version endpoint ([fb88a3d](https://github.com/jeroenflvr/fast-api/commit/fb88a3db9b95a222a732b4423386dae288d937d8))
- Versioned api endpoints ([d803221](https://github.com/jeroenflvr/fast-api/commit/d80322160c035c31dce7360cfa2571b6e137c161))
- Alignment and doc updates ([07ca7a4](https://github.com/jeroenflvr/fast-api/commit/07ca7a454989176e968f2ab70a8a8ddc661311f0))

### Tests

- Add unit tests ([66eaf87](https://github.com/jeroenflvr/fast-api/commit/66eaf87aa5fa9c447d18d31cfba20ebfb2a078a2))

## [0.1.17] - 2026-05-25

### Features

- Add arrow_ipc and hard cap page_size limit of 1M rows ([c7782dd](https://github.com/jeroenflvr/fast-api/commit/c7782dd062915fb9018c6dba59272e8b3890a97c))

## [0.1.16] - 2026-05-25

### Documentation

- Update readme ([954862d](https://github.com/jeroenflvr/fast-api/commit/954862d1748edb72f0425ffbb9dc3c066941dfae))

### Features

- Enable compression ([918bca1](https://github.com/jeroenflvr/fast-api/commit/918bca1b256988a368850c2d90cfec5e2e74f62d))
- Add group_by and distinct ([b154a94](https://github.com/jeroenflvr/fast-api/commit/b154a943871656f401c412d9f845b175f5495d5f))

## [0.1.15] - 2026-05-24

### Documentation

- Update readme for order by and limit ([861eded](https://github.com/jeroenflvr/fast-api/commit/861eded1b1bb57a06b12ae2674275cdc7c530fc0))
- Update readme ([1bbf8ef](https://github.com/jeroenflvr/fast-api/commit/1bbf8ef57b9c7ddfb5b3d5f22dac4d361e465fd8))

### Features

- Add limit and order by ([f202d8c](https://github.com/jeroenflvr/fast-api/commit/f202d8cc5a9edc46aa37963781082ad499a9d78a))

## [0.1.14] - 2026-05-24

### Miscellaneous

- Show endpoints per dataset at startup ([67e765d](https://github.com/jeroenflvr/fast-api/commit/67e765ddcbbce525cd834ae71129321a75f44666))

## [0.1.13] - 2026-05-24

### Features

- Add windows back to build targets for the "winners" ([c6c2537](https://github.com/jeroenflvr/fast-api/commit/c6c2537f69df933ae61c651ecf31dbc0096831f9))

### Refactor

- Move handlers to core ([57c49a0](https://github.com/jeroenflvr/fast-api/commit/57c49a0927d7117942e703958c65fa82f9ead453))

## [0.1.12] - 2026-05-24

### Documentation

- Update doc, banner, version ([8067330](https://github.com/jeroenflvr/fast-api/commit/806733048ed8142fe216fc3c6d1a4d3061f565f4))

## [0.1.11] - 2026-05-24

### Miscellaneous

- Rename python project ([cc76cf7](https://github.com/jeroenflvr/fast-api/commit/cc76cf7d00b81183948bc64d873d55f42b7cdea1))

## [0.1.10] - 2026-05-24

### Bug Fixes

- You know... ([184c326](https://github.com/jeroenflvr/fast-api/commit/184c32604e8ea9251034dc9031248bdb3a07a953))

### Features

- Add lazy mode for s3 data (parquet) ([2e858a8](https://github.com/jeroenflvr/fast-api/commit/2e858a827979f2c7645d2bdb2569f7703774fe1f))
- Count, with and without predicate ([212c444](https://github.com/jeroenflvr/fast-api/commit/212c4443bc5f96ff4755f7b333766e73af49025b))

## [0.1.9] - 2026-05-24

### CI

- Another duckdb lib build attempt ([5f74826](https://github.com/jeroenflvr/fast-api/commit/5f7482618ac2f3468fd304b98b0d8deb6f6c746e))

## [0.1.8] - 2026-05-24

### CI

- Another attempt on duckdb lib ([14fb834](https://github.com/jeroenflvr/fast-api/commit/14fb834376b03d5f1d3549f53feac9caf46c1591))

## [0.1.7] - 2026-05-24

### CI

- Still duckdb lib ([b08eb31](https://github.com/jeroenflvr/fast-api/commit/b08eb3136d04adef1d4598100d53af7beb36628f))

## [0.1.6] - 2026-05-24

### Bug Fixes

- *(multiple)* Ci platform, utf8view type casting, .. ([1f2a550](https://github.com/jeroenflvr/fast-api/commit/1f2a55016eb6590ba722e669603dedebda299775))

## [0.1.5] - 2026-05-24

### CI

- Only build on tag ([b6dbad8](https://github.com/jeroenflvr/fast-api/commit/b6dbad89a2144b9d866de9de7545a029ffb9eb5d))

## [0.1.4] - 2026-05-24

### CI

- Fix again ([ce73da5](https://github.com/jeroenflvr/fast-api/commit/ce73da5b8a6475420c327f4cfa63bf59b4c32bff))

## [0.1.3] - 2026-05-24

### CI

- Fix ([01a8b74](https://github.com/jeroenflvr/fast-api/commit/01a8b74bcacfba3bcac68bacfa0e196b3f31d5ee))

## [0.1.2] - 2026-05-23

### CI

- Fix sccache ([8d1f1d5](https://github.com/jeroenflvr/fast-api/commit/8d1f1d53ef92b8cd4e841fcfe550a4f1969910e3))

### Features

- Add lazy loading for larger than ram data ([d4f8320](https://github.com/jeroenflvr/fast-api/commit/d4f8320cf7946d3ca52a7a4e1e471eb3859a72dd))

## [0.1.1] - 2026-05-23

### CI

- Cleanup ([1eef801](https://github.com/jeroenflvr/fast-api/commit/1eef801020e9c3a1d139c5157387fa6cd31a748c))

## [0.1.0] - 2026-05-23

### Bug Fixes

- *(python)* Restore python-source files (excluded from .gitignore) ([ae2f48f](https://github.com/jeroenflvr/fast-api/commit/ae2f48f873d012b3c604e54a56d2ca94892460af))

### CI

- Update taskfile ([8185d1c](https://github.com/jeroenflvr/fast-api/commit/8185d1cce45e94df5a10e51908d3e70db3210550))
- Publish ([c7ee1f1](https://github.com/jeroenflvr/fast-api/commit/c7ee1f1023cb960837c69455301bcd8866fb7be9))
- Update pypi ([87f837f](https://github.com/jeroenflvr/fast-api/commit/87f837fd58d427bb16a2ef7897b4b46a1c799c2f))

### Documentation

- Update class __doc__ and add proxy prefix to handlers ([cad78f9](https://github.com/jeroenflvr/fast-api/commit/cad78f9fcdff7cc852efe6de797ca3a0a8059801))

### Features

- Read parquet and delta ([84da87c](https://github.com/jeroenflvr/fast-api/commit/84da87cb4260a20c47d4ebef17c7a1d33923f8f4))
- Refactor 2 bin ([a93ce62](https://github.com/jeroenflvr/fast-api/commit/a93ce6241e3a3489677adbfb9c15873551966c97))
- Combine duckdb and datafusion branches ([5ddf4dd](https://github.com/jeroenflvr/fast-api/commit/5ddf4dd3cca498f3eed153ffe15346852d779cb2))

### Miscellaneous

- Update taskfile ([d498fb6](https://github.com/jeroenflvr/fast-api/commit/d498fb63552778289ced6b526ed52b53aa238944))
- Tighten .gitignore for maturin build artifacts ([a63e1e5](https://github.com/jeroenflvr/fast-api/commit/a63e1e5d6b803616c43f44443ff0d827c29edd31))
- Cleanup ([4b34452](https://github.com/jeroenflvr/fast-api/commit/4b344520dfb4ffa08522c8d68409fd0d576cf065))
- Init ([9c3e139](https://github.com/jeroenflvr/fast-api/commit/9c3e13905c5f8afebf886ba2bfac2c716269488e))

### Refactor

- Change into workspace add python wrapper ([d9d38e3](https://github.com/jeroenflvr/fast-api/commit/d9d38e3bb85c36a35321450f07ee120af51716fb))

### Tests

- Update test queries ([e383992](https://github.com/jeroenflvr/fast-api/commit/e3839924b7133eb0a229699f52bb3b1afa072f6f))


