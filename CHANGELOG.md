# Changelog

All notable changes to this project are documented here.
Format based on [Keep a Changelog](https://keepachangelog.com/),
versioning follows [SemVer](https://semver.org/).
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


