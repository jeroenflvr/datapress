# Packaging & distribution

This directory holds everything needed to distribute the standalone
`datapress` CLI through common package managers. All channels install the
**prebuilt** binary from the GitHub release (both DuckDB + DataFusion backends
bundled) — none of them compile the heavy bundled DuckDB C++ from source.

| Channel        | User command                                              | Source of truth                         |
|----------------|-----------------------------------------------------------|-----------------------------------------|
| Install script | `curl -LsSf https://datap-rs.org/install.sh \| sh`        | [`install.sh`](../install.sh)           |
| Install script | `irm https://datap-rs.org/install.ps1 \| iex` (Windows)   | [`install.ps1`](../install.ps1)         |
| Homebrew       | `brew install jeroenflvr/tap/datapress`                   | [`homebrew/datapress.rb`](homebrew/datapress.rb) |
| winget         | `winget install datap-rs.DataPress`                       | [`winget/`](winget/)                    |
| Docker         | `docker run jeroenflvr/datapress`                         | [`docker/Dockerfile`](docker/Dockerfile) |

The release workflow ([`.github/workflows/publish.yml`](../.github/workflows/publish.yml))
already builds the CLI for `x86_64`/`aarch64` Linux, `aarch64` macOS, and
`x86_64` Windows, emits a `.sha256` next to each archive, and attaches the
archives **plus `install.sh` and `install.ps1`** to the GitHub Release. That
gives the install scripts a stable URL:

```
https://github.com/jeroenflvr/datapress/releases/latest/download/install.sh
```

## Hosting the short install URL (datap-rs.org)

To get the `curl … https://datap-rs.org/install.sh | sh` UX, copy `install.sh`
and `install.ps1` into the GitHub Pages repo that serves `datap-rs.org` (the
landing-site repo) so they are published at the apex domain. Re-copy them when
they change — they are version-agnostic and resolve the latest release at
runtime, so they rarely need updating.

Until then, the scripts also work directly from the release:

```bash
curl -LsSf https://github.com/jeroenflvr/datapress/releases/latest/download/install.sh | sh
```

## Homebrew

The formula lives in a **tap**, not homebrew-core.

1. Create a repo `homebrew-tap` under your account: `jeroenflvr/homebrew-tap`.
2. Add the formula at `Formula/datapress.rb` (copy [`homebrew/datapress.rb`](homebrew/datapress.rb)).
3. Users install with:

   ```bash
   brew tap jeroenflvr/tap
   brew install datapress
   # or in one line:
   brew install jeroenflvr/tap/datapress
   ```

### Automating the bump

Add a secret `HOMEBREW_TAP_TOKEN` (a fine-grained PAT with **Contents: write**
on the `homebrew-tap` repo). Once it is set, the `update-homebrew` job runs on
**every** `v*` release (i.e. every `task release`): it regenerates the formula
with [`homebrew/update-formula.sh`](homebrew/update-formula.sh) and pushes it.
If the secret is absent the job logs a warning and skips, so releases never
fail because of it.

To update by hand:

```bash
sh packaging/homebrew/update-formula.sh 0.4.4 Formula/datapress.rb
```

> Note: there is no prebuilt Intel macOS binary, so the formula errors out on
> Intel Macs with a hint to use `cargo install datapress`. Apple Silicon and
> Linux (x86_64/aarch64) are covered.

## winget

The manifests in [`winget/`](winget/) target `datap-rs.DataPress` and install
the Windows `.zip` as a portable command (`datapress`), which winget adds to
PATH via its portable links mechanism.

Publishing means opening a PR to
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) under
`manifests/d/datap-rs/DataPress/<version>/`.

### First-time bootstrap (required once)

The automated job below uses
[`winget-releaser`](https://github.com/vedantmgoyal9/winget-releaser), which
**only updates packages that already exist** in winget-pkgs. Until the first
version is merged it fails with:

```
Error: Package datap-rs.DataPress does not exist in the winget-pkgs repository.
Please add atleast one version of the package before using this action.
```

Submit the **initial** version by hand once (from a Windows machine with
[`wingetcreate`](https://github.com/microsoft/winget-create), e.g.
`winget install wingetcreate`):

```powershell
wingetcreate new `
  https://github.com/jeroenflvr/datapress/releases/download/v0.4.4/datapress-v0.4.4-x86_64-pc-windows-msvc.zip `
  --submit
```

Follow the prompts (it infers `datap-rs.DataPress`, version, and SHA-256 from
the release `.zip`). Once that PR is **merged** into winget-pkgs, every later
`v*` release is handled automatically by the job below.

### Automating the submission

Add a secret `WINGET_TOKEN` (a classic PAT with `public_repo` that can push to
**your fork** of `winget-pkgs`). Once it is set — **and the bootstrap version
above has been merged** — the `update-winget` job runs on **every** `v*`
release and uses `winget-releaser` to build the manifests from the release
`.zip` and open the PR automatically. If the secret is absent the job logs a
warning and skips.

The job is also gated on a repo **variable** `WINGET_BOOTSTRAPPED` so releases
stay green before the package exists. While the bootstrap PR is pending, leave
it unset and the job is skipped entirely. After that PR is merged, enable it
once: **Settings → Secrets and variables → Actions → Variables → New
repository variable → `WINGET_BOOTSTRAPPED` = `true`**.

To bump an existing package by hand instead:

```powershell
wingetcreate update datap-rs.DataPress `
  --version 0.4.4 `
  --urls https://github.com/jeroenflvr/datapress/releases/download/v0.4.4/datapress-v0.4.4-x86_64-pc-windows-msvc.zip `
  --submit
```

## Docker

The image at [`docker/Dockerfile`](docker/Dockerfile) installs the prebuilt
Linux release binary onto a `distroless/cc` base — no source build — so it
stays small and depends only on the GitHub release assets. It is multi-arch
(`linux/amd64` + `linux/arm64`); the binary links rustls + aws-lc-rs, so no
OpenSSL is needed at runtime.

```bash
# Pull and run (mount a config that sets listen = "0.0.0.0").
docker run --rm -p 8080:8080 \
  -v "$PWD/datasets.toml:/etc/datapress/datasets.toml:ro" \
  jeroenflvr/datapress:latest
```

The image reads its config from `DATAPRESS_CONFIG_FILE`
(`/etc/datapress/datasets.toml` by default). A container-ready example —
already set to `listen = "0.0.0.0"` — is in
[`docker/datasets.example.toml`](docker/datasets.example.toml).

Build locally:

```bash
docker buildx build --build-arg VERSION=0.4.5 \
  -f packaging/docker/Dockerfile -t datapress:0.4.5 --load .
```

### Automating the publish

Add two secrets, `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN` (a Docker Hub
access token with **Read & Write**). Once they are set, the `docker` job runs
on **every** `v*` release: it builds the multi-arch image from the release
binary and pushes `<username>/datapress` tagged `:<version>`, `:<major.minor>`,
and `:latest`. If the token is absent the job logs a warning and skips.

## Checksums

Every release archive ships with a matching `<archive>.sha256`. The install
scripts download and verify it automatically (and warn, rather than fail, only
for older releases published before checksums were added).
