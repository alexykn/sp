# Contributing to sph

> We love merge requests! This guide shows the fastest path from **idea** to **merged code**. Skip straight to the *Quick‑Start* if you just want to get going, or dive into the details below.

---

## ⏩ Quick‑Start

### 1. Fork, clone & branch
```bash
git clone https://github.com/<your-username>/sph.git
cd sph
git checkout -b feat/<topic>
```

### 2. Install Nightly Toolchain (for formatting)
```bash
rustup toolchain install nightly
```

### 3. Compile fast (uses stable toolchain from rust-toolchain.toml)
```bash
cargo check --workspace --all-targets
```

### 4. Format (uses nightly toolchain)
```bash
cargo +nightly fmt --all
```

### 5. Lint (uses stable toolchain)
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

### 6. Test (uses stable toolchain)
```bash
cargo test --workspace
```

### 7. Commit (Conventional + DCO)
```bash
git commit -s -m "feat(core): add new fetcher"
```

### 8. Push & open a Merge Request against `main`
```bash
git push origin feat/<topic>
# then open a merge request on GitHub
```

-----

## Project Layout

| Crate               | Role                                                     |
| ------------------- | -------------------------------------------------------- |
| **`sph-core`** | Library: dependency resolution, fetchers, install logic  |
| **`sph-cli`**  | Binary: user‑facing `sph` command                   |

All crates live in one Cargo **workspace**, so `cargo <cmd>` from the repo root affects everything.

-----

## Dev Environment

  * **Platform**: Development and execution require **macOS**.
  * **Rust (Build/Test)**: **Stable** toolchain, MSRV pinned in `rust-toolchain.toml` (currently *1.76.0*). Install via [rustup.rs][rustup.rs]. This is used by default for `cargo build`, `cargo check`, `cargo test`, etc.
  * **Rust (Format)**: **Nightly** toolchain is required *only* for formatting (`cargo fmt`) due to unstable options used in our `rustfmt.toml` configuration.
      * Install via: `rustup toolchain install nightly`
  * **Rust Components**: `rustfmt`, `clippy` – install via `rustup component add rustfmt clippy`. Make sure these components are available for *both* your default stable toolchain and the nightly toolchain.
  * **macOS System Tools**: Xcode Command Line Tools (provides C compiler, git, etc.). Install with `xcode-select --install`. You may also need `pkg-config` and `cmake` (e.g., install via [Homebrew][Homebrew]: `brew install pkg-config cmake`).

-----

## Coding Style

  * **Format** ‑ We use custom formatting rules (`rustfmt.toml`) which include unstable options (like `group_imports`, `imports_granularity`, `wrap_comments`, etc.). Applying these requires using the **nightly** toolchain. Format your code *before committing* using:
    ```bash
    cargo +nightly fmt --all
    ```
      * Ensure the nightly toolchain is installed (`rustup toolchain install nightly`).
      * CI runs `cargo +nightly fmt --all --check`, so MRs with incorrect formatting will fail.
  * **Lint** ‑ `cargo clippy … -D warnings`; annotate false positives with `#[allow()]` + comment. (This uses the default stable toolchain).
  * **API** ‑ follow the [Rust API Guidelines][Rust API Guidelines]; document every public item; avoid `unwrap()`.
  * **Dependencies** ‑ discuss new crates in the MR; future policy will use `cargo deny`.

-----

## Testing

  * Unit tests in modules, integration tests in `tests/`.
  * Aim to cover new code; bug‑fix MRs **must** include a failing test that passes after the fix.
  * `cargo test --workspace` must pass (uses the default stable toolchain).

-----

## Git & Commits

  * **Fork** the repo on GitHub and add your remote if you haven’t already.
  * **Branches**: use feature branches like `feat/…`, `fix/…`, `docs/…`, `test/…`.
  * **Conventional Commits** preferred (`feat(core): add bottle caching`).
  * **DCO**: add `-s` flag (`git commit -s …`).
  * Keep commits atomic; squash fix‑ups before marking the MR ready.

-----

## Merge‑Request Flow

1.  Sync with `main`; rebase preferred.
2.  Ensure your code is formatted correctly with `cargo +nightly fmt --all`.
3.  Ensure CI is green (build, fmt check, clippy, tests on macOS using appropriate toolchains).
4.  Fill out the MR template; explain *why* + *how*.
5.  Respond to review comments promptly – we’re friendly, promise!
6.  Maintainers will *Squash & Merge* (unless history is already clean).

-----

## Reporting Issues

  * **Bug** – include repro steps, expected vs. actual, macOS version & architecture (Intel/ARM).
  * **Feature** – explain use‑case, alternatives, and willingness to implement.
  * **Security** – email maintainers privately; do **not** file a public issue.

-----

## License & DCO

By submitting code you agree to the BSD‑3‑Clause license and certify the [Developer Certificate of Origin][Developer Certificate of Origin].

-----

## Code of Conduct

We follow the [Contributor Covenant][Contributor Covenant]; be kind and inclusive. Report misconduct privately to the core team.

-----

Happy coding – and thanks for making sph better! ✨

[rustup.rs]: https://rustup.rs/
[homebrew]: https://brew.sh/
[Rust API Guidelines]: https://rust-lang.github.io/api-guidelines/
[Developer Certificate of Origin]: https://developercertificate.org/
[Contributor Covenant]: https://www.contributor-covenant.org/version/2/1/code_of_conduct/

