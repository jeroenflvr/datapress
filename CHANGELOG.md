# Changelog

All notable changes to this project are documented here.
Format based on [Keep a Changelog](https://keepachangelog.com/),
versioning follows [SemVer](https://semver.org/).
## [0.4.19] - 2026-06-13

### Bug Fixes

- *(ci)* Build failures for windows (duckdb dep versions) ([e2a98fc](https://github.com/jeroenflvr/datapress/commit/e2a98fce63a5cc9dee6b5b745716fd774f3d706b))

## [0.4.18] - 2026-06-13

### Bug Fixes

- Correct empty data handling with datafusion - log and skip ([19791e1](https://github.com/jeroenflvr/datapress/commit/19791e17e4545cf231e053e793cb53ab1919e8c9))

### Miscellaneous

- Update crates from audit failures ([7bdd405](https://github.com/jeroenflvr/datapress/commit/7bdd40521140b7f7ee492dc32d0a1d11be2de020))

## [0.4.17] - 2026-06-12

### Bug Fixes

- Reload on changed filenames (parquet on s3) ([a327c4b](https://github.com/jeroenflvr/datapress/commit/a327c4b67b0ed01ef8a62474371604f2cf5813e9))

### Documentation

- Add diagrams for typical usage + why rust ([19a5d0d](https://github.com/jeroenflvr/datapress/commit/19a5d0dabedcc7eeb5b4a9d152cef0448128ff6f))
- Update datafusion tuning section, add troubleshooting ([f3f1404](https://github.com/jeroenflvr/datapress/commit/f3f14048bf4b655b09b9190ed489703d385076a8))
- Add links ([85e71bc](https://github.com/jeroenflvr/datapress/commit/85e71bc40d1cbd23874472595fc041cde1a4c70a))

### Features

- Allow describe in sql endpoint ([b4d6b8b](https://github.com/jeroenflvr/datapress/commit/b4d6b8b18af2a25c852e035f929d524e607e1090))

## [0.4.16] - 2026-06-11

### Bug Fixes

- *(ci)* Dependency tree ([45bf9ef](https://github.com/jeroenflvr/datapress/commit/45bf9efd9dfba7d57a330480f41eed4664e069a2))

## [0.4.15] - 2026-06-11

### Bug Fixes

- Update with lazy forced on s3 ([04256df](https://github.com/jeroenflvr/datapress/commit/04256dfd5dcf154c649b0b5917ee09fd348e1a16))
- Drop another no_run ([9927992](https://github.com/jeroenflvr/datapress/commit/9927992ba44a15cdfc0c0cb7c15b84132499e964))
- *(doc)* Remove no_run marker ([20f0420](https://github.com/jeroenflvr/datapress/commit/20f0420e42360b61daab608b89085633a503d19d))

### Features

- Static linux binaries ([7ec6962](https://github.com/jeroenflvr/datapress/commit/7ec6962c730e357550b1a09a6a85c82f248c1d2c))
- Lazy forced on size limit ([b9f8be4](https://github.com/jeroenflvr/datapress/commit/b9f8be4d9c64a13e8f8467d6b93a1d15e36e61f4))

## [0.4.14] - 2026-06-08

### Bug Fixes

- *(ci)* Deduplicate embedded duckdb wasm ([353e351](https://github.com/jeroenflvr/datapress/commit/353e35111a4b81ed3ce23ab100851b99f6d38cf3))

## [0.4.13] - 2026-06-08

### Bug Fixes

- *(client-py)* Use valid pypi troveclassifier ([e0b66f9](https://github.com/jeroenflvr/datapress/commit/e0b66f9aee8f6afffdf87dbd4c2eed4420124a01))

## [0.4.12] - 2026-06-08

### Features

- Add client ([5a69f79](https://github.com/jeroenflvr/datapress/commit/5a69f7977c8b0424fbec2bf96568bd6fb935bae4))

## [0.4.11] - 2026-06-07

### Features

- Add having and sql-to-json request ([091aa42](https://github.com/jeroenflvr/datapress/commit/091aa42189cbac54542024099b3637a386bbf12f))

### Styling

- *(docs)* Update links, add having ([7eb1437](https://github.com/jeroenflvr/datapress/commit/7eb1437cdcf0d163f549cfc6c5dbfc807949a3c8))

## [0.4.10] - 2026-06-07

### Bug Fixes

- To include utf8View support in js, build and vendor from https://github.com/apache/arrow-js/pull/320 ([2347494](https://github.com/jeroenflvr/datapress/commit/2347494c099af5530b2fc74f53505de949d6556a))

## [0.4.9] - 2026-06-06

### Miscellaneous

- Add more todos re protection against ddos ([0437c63](https://github.com/jeroenflvr/datapress/commit/0437c637204d9d3932a1eed786e57a235a15a77e))
- Default to datafusion ([a4e7878](https://github.com/jeroenflvr/datapress/commit/a4e78786ab35c2bf05ca76d5bd4721923d6089c8))
- Update cli binary with updated template ([5181812](https://github.com/jeroenflvr/datapress/commit/5181812628bcbb76dc77be19e4490cc66ffd92fe))

## [0.4.8] - 2026-06-06

### Bug Fixes

- *(ci)* Add resiliency ([10cd3f6](https://github.com/jeroenflvr/datapress/commit/10cd3f68791150696302bdafb461558771b2f8b6))

### Documentation

- Update disable_ssl to uppercase ([68716ce](https://github.com/jeroenflvr/datapress/commit/68716ce68360af46f1d7a73143823ab8299bb28c))
- Update disable_ssl for remote hosts in dev envs ([3dec17e](https://github.com/jeroenflvr/datapress/commit/3dec17e95e9a47caead225cc89e595c4c386aa23))

### Miscellaneous

- Update examples ([a858d2c](https://github.com/jeroenflvr/datapress/commit/a858d2c93d03b1a18b6910b6e4388f322e5f95ae))

## [0.4.7] - 2026-06-06

### Features

- Allow to bypass compression from browser with json ([61e7123](https://github.com/jeroenflvr/datapress/commit/61e7123f932654e30e82505d66224a40246a1a62))

### Performance

- Improve arrow transfer speed by dropping redundant compression (huge win) ([1f32f7b](https://github.com/jeroenflvr/datapress/commit/1f32f7bb895a03431dc59a756bd35622de0d26c6))

## [0.4.6] - 2026-06-06

### Bug Fixes

- *(style)* Logo and color ([690db8f](https://github.com/jeroenflvr/datapress/commit/690db8fb4fa49aca0b8efcaf993c9722f68df3a8))

### CI

- Add docker build ([5755043](https://github.com/jeroenflvr/datapress/commit/57550432fd7ef694930e2bc8922228df674507e8))

### Documentation

- Remove edit icon ([42c2429](https://github.com/jeroenflvr/datapress/commit/42c24290f602a51dfe45edfc0ca56e880c26a890))

### Features

- Embed json/raw sql interface in explorer ([1af356c](https://github.com/jeroenflvr/datapress/commit/1af356ca426b191c61f784912702bf3bdd2bc189))
- Branding binary ([186b39b](https://github.com/jeroenflvr/datapress/commit/186b39b71d90e6d8991da0b7f77aee9742adbc4c))
- *(style)* Duckdb wasm branding ([ad9cfff](https://github.com/jeroenflvr/datapress/commit/ad9cfffc2aac93cc92845fdccc6044e9da86d126))
- Add export query results from explore endpoint ([fb1b3c4](https://github.com/jeroenflvr/datapress/commit/fb1b3c42f5a2104e2e1061719b0b0edaa90072d5))

### Miscellaneous

- Add sql block to datasets.toml template ([3d32813](https://github.com/jeroenflvr/datapress/commit/3d3281333820b700c4bd043f957dffa172af8951))

### Styling

- Update branding ([47619af](https://github.com/jeroenflvr/datapress/commit/47619afaadb55b8e1746ac9d48de953552b304a5))
- Update mkdocs theme to match presentation ([04dadbe](https://github.com/jeroenflvr/datapress/commit/04dadbefa1b2494595f4c3747ab7f36b49e145df))

## [0.4.5] - 2026-06-06

### CI

- Update installation methods ([5fb7e51](https://github.com/jeroenflvr/datapress/commit/5fb7e51133d12f7c65631fcda2d99436ad8befe9))

### Documentation

- Update presentation link ([3aac9aa](https://github.com/jeroenflvr/datapress/commit/3aac9aad468c149ac41531e645defcb75cb8bd63))

## [0.4.4] - 2026-06-05

### Documentation

- Update python README with sql endpoint and admin_token override ([2c67c43](https://github.com/jeroenflvr/datapress/commit/2c67c436dafb166229df601910b561785849a276))

## [0.4.3] - 2026-06-05

### Features

- Allow admin_token to be set from python, not exclusively via env var ([f08922e](https://github.com/jeroenflvr/datapress/commit/f08922e1fbfa7a63c52216c35d6419d3da2bb3ca))

## [0.4.2] - 2026-06-05

### Bug Fixes

- *(ci)* Free some diskspace before the full build ([c185884](https://github.com/jeroenflvr/datapress/commit/c185884926e67a51fa693455f77b200258a8b14a))

## [0.4.1] - 2026-06-04

### Miscellaneous

- Rename git repo to datapress ([a5b5843](https://github.com/jeroenflvr/datapress/commit/a5b5843d77bf42a3cdc54caf5ddfed995f153cf2))

## [0.4.0] - 2026-06-04

### Bug Fixes

- Update swagger, make sql case insensitive for datafusion (similar to duckdb) ([a444a0a](https://github.com/jeroenflvr/datapress/commit/a444a0a63728f7483f7f267ebb71b41878033814))

### Documentation

- Update for seo ([03ac000](https://github.com/jeroenflvr/datapress/commit/03ac00038cdc825c58ddba6ad895cb3ed407a395))

### Features

- Add sql endpoint ([e5546d8](https://github.com/jeroenflvr/datapress/commit/e5546d851fbdbac267ca0fc4cc6e48af06076cbb))

## [0.3.3] - 2026-06-03

### Documentation

- Update we're already doing delta with datafusion backend too ([011ec82](https://github.com/jeroenflvr/datapress/commit/011ec8225b24a8dde0a2795d187d4b424d1ab15b))

### Refactor

- More perf improvements (cow) ([0a8d083](https://github.com/jeroenflvr/datapress/commit/0a8d083478b57b53e6b571b471b2f0689fb990d5))

### Bench

- Optimize vectorized operations on json data ([28d9908](https://github.com/jeroenflvr/datapress/commit/28d990821d1862dfc43c04d5a121359642f274d2))

## [0.3.2] - 2026-06-03

### Bug Fixes

- Lazy loading for duckdb backend ([0fa5da5](https://github.com/jeroenflvr/datapress/commit/0fa5da503d806f55d7c42fb86332f417fc7a3589))

## [0.3.1] - 2026-06-03

### Bug Fixes

- *(ci)* Include explorer feature in every build ([c5db03f](https://github.com/jeroenflvr/datapress/commit/c5db03f5d7743f960b9b6111c20b53c9c807ae31))

## [0.3.0] - 2026-06-02

### Documentation

- Update name ([5816d4b](https://github.com/jeroenflvr/datapress/commit/5816d4b2eda0534e0a30b8c1a44a848a709e7330))

### Features

- *(explorer)* Add a duckdb wasm terminal ([a8de1e6](https://github.com/jeroenflvr/datapress/commit/a8de1e66148f1ffee6c7e45dc62593308d1cbb67))
- Add embedded explorer website with tables etc, and duckdb wasm terminal ([9469336](https://github.com/jeroenflvr/datapress/commit/9469336db8568f9cd4a6b837002538cfd65903bb))

## [0.2.18] - 2026-06-02

### Bug Fixes

- *(ci)* Cargo clippy warnings ([b8342c7](https://github.com/jeroenflvr/datapress/commit/b8342c7b58468a0c1216305588dc43869d1bbedb))

### Documentation

- Add duckdb-wasm instance to mkdocs ([45c1f61](https://github.com/jeroenflvr/datapress/commit/45c1f61fa3ca2c7d642e64116b2b8fe3f7e438db))

### Features

- Add streaming parquet endpoints for duckdb client ([4d15a64](https://github.com/jeroenflvr/datapress/commit/4d15a647339a8bfe2405345f7e57011088aabb96))
- Align s3 config between datafusion and duckdb ([36b915d](https://github.com/jeroenflvr/datapress/commit/36b915db9e4c185aa6ab3b39b8ca65d994ecc8aa))

## [0.2.17] - 2026-06-01

### Features

- Pass in callable for hmac secrets fetching with python ([8757887](https://github.com/jeroenflvr/datapress/commit/8757887741e746b071746e68fba199031304754a))

### Styling

- Better version badge in mkdocs (css) ([345ea2d](https://github.com/jeroenflvr/datapress/commit/345ea2d174c1844443ad208fa9a10926710db01d))

## [0.2.16] - 2026-05-31

### Documentation

- Update readme and doc links ([8fd6701](https://github.com/jeroenflvr/datapress/commit/8fd67013b791cd32032cfb372970e5812abc8aee))

### Features

- Add option to generate dataseets.toml template from datapress ([878f1a2](https://github.com/jeroenflvr/datapress/commit/878f1a2b60ba6b578b20a0c147ce60f2d243112d))

## [0.2.15] - 2026-05-31

### Features

- Add cli tool, installable binary ([6ac8538](https://github.com/jeroenflvr/datapress/commit/6ac8538ffcb189ff63a9d9acff1f7cf8fc0eb386))

## [0.2.14] - 2026-05-31

### Bug Fixes

- Oidc on swagger ui ([6a8c0bc](https://github.com/jeroenflvr/datapress/commit/6a8c0bc0daa8d968d72019a7e864e04a854853df))
- *(sec)* Fix sec bugs ([9cc09f3](https://github.com/jeroenflvr/datapress/commit/9cc09f32cbe4f9ba443770eb3249743f74fd2244))

## [0.2.13] - 2026-05-31

### Bug Fixes

- Swagger auth ([0307c7a](https://github.com/jeroenflvr/datapress/commit/0307c7a4070a499fd08750d3045a732eb894655d))

## [0.2.12] - 2026-05-31

### Features

- Add auth to swagger ui from python ([d5a3ada](https://github.com/jeroenflvr/datapress/commit/d5a3adaf343cf09e2a242e0040fcc8c66233bc5f))

## [0.2.11] - 2026-05-31

### Bug Fixes

- *(oidc)* Make sub claim optional ([d0fc43d](https://github.com/jeroenflvr/datapress/commit/d0fc43d9d46965d45b9555f72749e8e9c5aba3cc))

## [0.2.10] - 2026-05-31

### Bug Fixes

- *(oidc)* Aud and scope are allowed to return both arrays and strings ([53074e4](https://github.com/jeroenflvr/datapress/commit/53074e48345d8cd442518a6078d375101fd24070))

## [0.2.9] - 2026-05-31

### Documentation

- Update links (pypi) ([31c81b5](https://github.com/jeroenflvr/datapress/commit/31c81b50fadb0ccf1be4906863cedeccc92feb4d))
- Update entry page ([aee0a87](https://github.com/jeroenflvr/datapress/commit/aee0a87f68688073558bedca9ab58418d28c1bfd))

### Miscellaneous

- *(doc)* Fmt ([ea21b58](https://github.com/jeroenflvr/datapress/commit/ea21b5809f2a5934db45c8e6ee660f980b31855c))
- *(doc)* Fmt ([438ba1c](https://github.com/jeroenflvr/datapress/commit/438ba1c54cb4bc87f2c351ef5b20af88e8a07afe))

## [0.2.8] - 2026-05-31

### Bug Fixes

- Oidc crypto provider ([5a61131](https://github.com/jeroenflvr/datapress/commit/5a611318e2be1be20f3ebbf7bf792fa3c47c2738))

### CI

- Publish mkdocs site ([29e0bb8](https://github.com/jeroenflvr/datapress/commit/29e0bb893db8c982f3c23975aecde4b46dc62e32))

### Documentation

- Update for oidc integration ([acfcdff](https://github.com/jeroenflvr/datapress/commit/acfcdff0f66252bc836164d6dc1dfee7209a9aab))

## [0.2.7] - 2026-05-31

### Bug Fixes

- S3 secret config for duckdb ([8b0dd3f](https://github.com/jeroenflvr/datapress/commit/8b0dd3fdd6f94a5c65713e41f5a36da5c517518c))

### CI

- Add docs and mkdocs to develop build ([79db0d4](https://github.com/jeroenflvr/datapress/commit/79db0d40f70275e7c2a3f9416172d6438387a4cd))

## [0.2.6] - 2026-05-30

### Bug Fixes

- Clippy ([299a67d](https://github.com/jeroenflvr/datapress/commit/299a67d060794c652555b7b5b66d8d49a6f63f8f))

### Features

- Implement quach protocol for duckdb (seriously) ([746675e](https://github.com/jeroenflvr/datapress/commit/746675ebdef0b72a638b3689acc192a1ac292407))
- When duckdb, allow new quack for direct connection ([fc8f845](https://github.com/jeroenflvr/datapress/commit/fc8f845171cae66acabb706792794e7853fb2002))

### Miscellaneous

- *(deps)* Bump the rust-dependencies group across 1 directory with 3 updates ([df3ed0e](https://github.com/jeroenflvr/datapress/commit/df3ed0ec85837d33efd1b0f27045031dc74f77f6))
- *(deps)* Bump the github-actions group across 1 directory with 5 updates ([21c685f](https://github.com/jeroenflvr/datapress/commit/21c685fc5276ea0d62eabaa8bb447427d392871b))

## [0.2.5] - 2026-05-30

### Documentation

- Updating arrow ipc ([6a21946](https://github.com/jeroenflvr/datapress/commit/6a2194672a548e981254be25113d6145a64f864a))
- Clarify different compression handlers: gzip/deflate mostly handled automatically, brotli needs additional handling ([5053a4d](https://github.com/jeroenflvr/datapress/commit/5053a4d631d17ff5ad88735c7c31dfe835877573))
- Clarify request and response config ([ac9aa30](https://github.com/jeroenflvr/datapress/commit/ac9aa30f5c60fb3ac372dc709f026306218418b9))
- Update mkdocs and add logo ([b6661ca](https://github.com/jeroenflvr/datapress/commit/b6661ca5b91bcd4cb862599d89350d1f749b5763))
- Update on double buffer handling ([5aa28c8](https://github.com/jeroenflvr/datapress/commit/5aa28c8141e837a8f644c991e734ff965bbaabfe))

### Features

- Update to full stream support (first step) ([58a6b47](https://github.com/jeroenflvr/datapress/commit/58a6b479dd274614d546125bbf3c9d1840f50a81))

### Miscellaneous

- Rust fmt ([9eddee4](https://github.com/jeroenflvr/datapress/commit/9eddee4bd0f0392969b814a49b9610fa34c84f52))

## [0.2.4] - 2026-05-30

### Miscellaneous

- Always build with metrics ([ec1c7ee](https://github.com/jeroenflvr/datapress/commit/ec1c7eec76e21333eb504f60803b6b59a12d85b7))
- Pin mkdocs version to avoid 2.0 breaking changes ([d17e509](https://github.com/jeroenflvr/datapress/commit/d17e50952d1cf0cdec3b95b4d160297030309adb))
- Cleanup projects and align crates ([74f6d6d](https://github.com/jeroenflvr/datapress/commit/74f6d6d2443a759f914747952f9fe2dbb623c9d2))
- Update community standards ([9d35495](https://github.com/jeroenflvr/datapress/commit/9d354955609512c74d5f7078ca73ec010f050eb7))

## [0.2.3] - 2026-05-29

### Bug Fixes

- Group_by on duckdb backend ([b7cfd0f](https://github.com/jeroenflvr/datapress/commit/b7cfd0f49ae441ef3cea31e6bd0a84f910042c51))

### CI

- Add dependency audit ([99665a4](https://github.com/jeroenflvr/datapress/commit/99665a4149119042fa99fec2c6fc00c01d121ef5))

### Documentation

- Update python bindings doc ([b811f6a](https://github.com/jeroenflvr/datapress/commit/b811f6a6ed53cd79d11bb872ac2c0623aaf9a298))

### Features

- Raising community standards ([d5010db](https://github.com/jeroenflvr/datapress/commit/d5010db7e334332fcdf87f8b81a927b02c83678b))
- Add prometheus exports ([3335c93](https://github.com/jeroenflvr/datapress/commit/3335c93d598a950c71089f89f40041c7a196fa2f))
- Support hive-style partitioning of parquet data ([c171b23](https://github.com/jeroenflvr/datapress/commit/c171b23e1bf684b66e95fa8c1aec0e3a29ce5a97))

## [0.2.2] - 2026-05-28

### Bug Fixes

- *(ci)* Don't trigger on main ([80b54ef](https://github.com/jeroenflvr/datapress/commit/80b54ef39ac6c6e79558afb6a5bb4e568b036b90))

## [0.2.1] - 2026-05-28

### Bug Fixes

- *(tests)* Done ([04e9ff2](https://github.com/jeroenflvr/datapress/commit/04e9ff2b5084df11d5c35665aab5fc45517d7ba4))

## [0.2.0] - 2026-05-28

### Documentation

- Update auth doc ([9f83459](https://github.com/jeroenflvr/datapress/commit/9f83459e051b4cd2e061644b8ff0b2a3d188cbf7))

### Features

- Add pluggable auth ([510592f](https://github.com/jeroenflvr/datapress/commit/510592f3c6753a57507ae8ea2741378ff0db7f4c))
- Add docs/mkdocs/version/.. ([5e2cb04](https://github.com/jeroenflvr/datapress/commit/5e2cb0406ecacf0df6123386aa27ae89edd12280))

## [0.1.18] - 2026-05-27

### Documentation

- Add mkdocs, embedded ([8d3ef25](https://github.com/jeroenflvr/datapress/commit/8d3ef258f44e3b1a6ca129d72fd06553f7a840d7))

### Features

- Add version endpoint ([fb88a3d](https://github.com/jeroenflvr/datapress/commit/fb88a3db9b95a222a732b4423386dae288d937d8))
- Versioned api endpoints ([d803221](https://github.com/jeroenflvr/datapress/commit/d80322160c035c31dce7360cfa2571b6e137c161))
- Alignment and doc updates ([07ca7a4](https://github.com/jeroenflvr/datapress/commit/07ca7a454989176e968f2ab70a8a8ddc661311f0))

### Tests

- Add unit tests ([66eaf87](https://github.com/jeroenflvr/datapress/commit/66eaf87aa5fa9c447d18d31cfba20ebfb2a078a2))

## [0.1.17] - 2026-05-25

### Features

- Add arrow_ipc and hard cap page_size limit of 1M rows ([c7782dd](https://github.com/jeroenflvr/datapress/commit/c7782dd062915fb9018c6dba59272e8b3890a97c))

## [0.1.16] - 2026-05-25

### Documentation

- Update readme ([954862d](https://github.com/jeroenflvr/datapress/commit/954862d1748edb72f0425ffbb9dc3c066941dfae))

### Features

- Enable compression ([918bca1](https://github.com/jeroenflvr/datapress/commit/918bca1b256988a368850c2d90cfec5e2e74f62d))
- Add group_by and distinct ([b154a94](https://github.com/jeroenflvr/datapress/commit/b154a943871656f401c412d9f845b175f5495d5f))

## [0.1.15] - 2026-05-24

### Documentation

- Update readme for order by and limit ([861eded](https://github.com/jeroenflvr/datapress/commit/861eded1b1bb57a06b12ae2674275cdc7c530fc0))
- Update readme ([1bbf8ef](https://github.com/jeroenflvr/datapress/commit/1bbf8ef57b9c7ddfb5b3d5f22dac4d361e465fd8))

### Features

- Add limit and order by ([f202d8c](https://github.com/jeroenflvr/datapress/commit/f202d8cc5a9edc46aa37963781082ad499a9d78a))

## [0.1.14] - 2026-05-24

### Miscellaneous

- Show endpoints per dataset at startup ([67e765d](https://github.com/jeroenflvr/datapress/commit/67e765ddcbbce525cd834ae71129321a75f44666))

## [0.1.13] - 2026-05-24

### Features

- Add windows back to build targets for the "winners" ([c6c2537](https://github.com/jeroenflvr/datapress/commit/c6c2537f69df933ae61c651ecf31dbc0096831f9))

### Refactor

- Move handlers to core ([57c49a0](https://github.com/jeroenflvr/datapress/commit/57c49a0927d7117942e703958c65fa82f9ead453))

## [0.1.12] - 2026-05-24

### Documentation

- Update doc, banner, version ([8067330](https://github.com/jeroenflvr/datapress/commit/806733048ed8142fe216fc3c6d1a4d3061f565f4))

## [0.1.11] - 2026-05-24

### Miscellaneous

- Rename python project ([cc76cf7](https://github.com/jeroenflvr/datapress/commit/cc76cf7d00b81183948bc64d873d55f42b7cdea1))

## [0.1.10] - 2026-05-24

### Bug Fixes

- You know... ([184c326](https://github.com/jeroenflvr/datapress/commit/184c32604e8ea9251034dc9031248bdb3a07a953))

### Features

- Add lazy mode for s3 data (parquet) ([2e858a8](https://github.com/jeroenflvr/datapress/commit/2e858a827979f2c7645d2bdb2569f7703774fe1f))
- Count, with and without predicate ([212c444](https://github.com/jeroenflvr/datapress/commit/212c4443bc5f96ff4755f7b333766e73af49025b))

## [0.1.9] - 2026-05-24

### CI

- Another duckdb lib build attempt ([5f74826](https://github.com/jeroenflvr/datapress/commit/5f7482618ac2f3468fd304b98b0d8deb6f6c746e))

## [0.1.8] - 2026-05-24

### CI

- Another attempt on duckdb lib ([14fb834](https://github.com/jeroenflvr/datapress/commit/14fb834376b03d5f1d3549f53feac9caf46c1591))

## [0.1.7] - 2026-05-24

### CI

- Still duckdb lib ([b08eb31](https://github.com/jeroenflvr/datapress/commit/b08eb3136d04adef1d4598100d53af7beb36628f))

## [0.1.6] - 2026-05-24

### Bug Fixes

- *(multiple)* Ci platform, utf8view type casting, .. ([1f2a550](https://github.com/jeroenflvr/datapress/commit/1f2a55016eb6590ba722e669603dedebda299775))

## [0.1.5] - 2026-05-24

### CI

- Only build on tag ([b6dbad8](https://github.com/jeroenflvr/datapress/commit/b6dbad89a2144b9d866de9de7545a029ffb9eb5d))

## [0.1.4] - 2026-05-24

### CI

- Fix again ([ce73da5](https://github.com/jeroenflvr/datapress/commit/ce73da5b8a6475420c327f4cfa63bf59b4c32bff))

## [0.1.3] - 2026-05-24

### CI

- Fix ([01a8b74](https://github.com/jeroenflvr/datapress/commit/01a8b74bcacfba3bcac68bacfa0e196b3f31d5ee))

## [0.1.2] - 2026-05-23

### CI

- Fix sccache ([8d1f1d5](https://github.com/jeroenflvr/datapress/commit/8d1f1d53ef92b8cd4e841fcfe550a4f1969910e3))

### Features

- Add lazy loading for larger than ram data ([d4f8320](https://github.com/jeroenflvr/datapress/commit/d4f8320cf7946d3ca52a7a4e1e471eb3859a72dd))

## [0.1.1] - 2026-05-23

### CI

- Cleanup ([1eef801](https://github.com/jeroenflvr/datapress/commit/1eef801020e9c3a1d139c5157387fa6cd31a748c))

## [0.1.0] - 2026-05-23

### Bug Fixes

- *(python)* Restore python-source files (excluded from .gitignore) ([ae2f48f](https://github.com/jeroenflvr/datapress/commit/ae2f48f873d012b3c604e54a56d2ca94892460af))

### CI

- Update taskfile ([8185d1c](https://github.com/jeroenflvr/datapress/commit/8185d1cce45e94df5a10e51908d3e70db3210550))
- Publish ([c7ee1f1](https://github.com/jeroenflvr/datapress/commit/c7ee1f1023cb960837c69455301bcd8866fb7be9))
- Update pypi ([87f837f](https://github.com/jeroenflvr/datapress/commit/87f837fd58d427bb16a2ef7897b4b46a1c799c2f))

### Documentation

- Update class __doc__ and add proxy prefix to handlers ([cad78f9](https://github.com/jeroenflvr/datapress/commit/cad78f9fcdff7cc852efe6de797ca3a0a8059801))

### Features

- Read parquet and delta ([84da87c](https://github.com/jeroenflvr/datapress/commit/84da87cb4260a20c47d4ebef17c7a1d33923f8f4))
- Refactor 2 bin ([a93ce62](https://github.com/jeroenflvr/datapress/commit/a93ce6241e3a3489677adbfb9c15873551966c97))
- Combine duckdb and datafusion branches ([5ddf4dd](https://github.com/jeroenflvr/datapress/commit/5ddf4dd3cca498f3eed153ffe15346852d779cb2))

### Miscellaneous

- Update taskfile ([d498fb6](https://github.com/jeroenflvr/datapress/commit/d498fb63552778289ced6b526ed52b53aa238944))
- Tighten .gitignore for maturin build artifacts ([a63e1e5](https://github.com/jeroenflvr/datapress/commit/a63e1e5d6b803616c43f44443ff0d827c29edd31))
- Cleanup ([4b34452](https://github.com/jeroenflvr/datapress/commit/4b344520dfb4ffa08522c8d68409fd0d576cf065))
- Init ([9c3e139](https://github.com/jeroenflvr/datapress/commit/9c3e13905c5f8afebf886ba2bfac2c716269488e))

### Refactor

- Change into workspace add python wrapper ([d9d38e3](https://github.com/jeroenflvr/datapress/commit/d9d38e3bb85c36a35321450f07ee120af51716fb))

### Tests

- Update test queries ([e383992](https://github.com/jeroenflvr/datapress/commit/e3839924b7133eb0a229699f52bb3b1afa072f6f))


