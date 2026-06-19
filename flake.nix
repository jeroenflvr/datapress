{
  description = "DataPress — multi-backend (DuckDB / DataFusion) dataset HTTP server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pinned via rust-overlay. We pull in the components rust-analyzer
        # needs to work end-to-end inside the editor:
        #   - rust-src      → "go to definition" into std
        #   - rust-analyzer → the RA server binary itself (matches toolchain)
        #   - clippy/rustfmt→ on-save lint + format
        # edition = "2024" / resolver = "3" in Cargo.toml require Rust ≥ 1.85,
        # which `stable.latest` comfortably satisfies.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };

        # bindgen (used by some *-sys crates) locates libclang at runtime via
        # LIBCLANG_PATH. Harmless when unused, required when it is.
        libclang = pkgs.llvmPackages.libclang;
      in
      {
        devShells.default = pkgs.mkShell {
          # Build-host tools: compilers + code-generators + their deps.
          nativeBuildInputs = with pkgs; [
            rustToolchain

            pkg-config
            cmake # aws-lc-sys (rustls' crypto backend) builds with CMake
            perl # assembly generation for aws-lc-sys / ring
            clang # C/C++ frontend for bindgen + the `cc` crate

            # Python side: pyo3 probes an interpreter for its build config,
            # maturin builds the wheel, uv drives the venv + `task py:*`.
            python3
            maturin
            uv

            go-task # the repo's Taskfile runner (`task build`, `task check`, …)
          ];

          # Libraries linked into the produced artifacts.
          buildInputs = with pkgs; [
            stdenv.cc.cc.lib # libstdc++ for the bundled DuckDB C++ objects
            openssl # safety net for any transitive openssl-sys pull
            zlib
          ];

          # bindgen → libclang.so
          LIBCLANG_PATH = "${libclang.lib}/lib";

          # rust-analyzer → std sources (also discoverable, but explicit is nice).
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          # pyo3 picks this interpreter when probing its build config so the
          # `extension-module` cdylib resolves the right abi3 settings.
          PYO3_PYTHON = "${pkgs.python3}/bin/python3";

          shellHook = ''
            echo "❄️  datapress dev shell — $(rustc --version)"
          '';
        };
      });
}
