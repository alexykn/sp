**IMPORTANT WARNING: ALPHA SOFTWARE**

**Sapphire is currently in an ALPHA stage of development. It is experimental and potentially unstable. While many features are functional, exercise caution, especially when installing complex or foundational packages. Use at your own discretion and risk!**

*I do not know if I will ever manage to finish or maintain this, it is a bit much for one person as a side project.*

---

## Introduction

Sapphire is an experimental package manager written in Rust, inspired by Homebrew. It aims to provide a way to install and manage command-line software (formulae) and eventually applications (casks) on macOS and Linux, leveraging modern tools and technologies.

It directly interacts with the official Homebrew repositories for formulae, bottles, and casks. Adding custom taps is not yet supported.

The project is split into:

- `sapphire-core`: The underlying library handling fetching, dependency resolution, building, installation, etc.
- `sapphire-cli`: The command-line interface tool that users interact with.

## Current State

- **Formulae Installation:**  
  - Bottle support now allows installing and uninstalling multiple bottles in one command.  
  - Concurrent downloads and installations make operations noticeably faster.  
  - Source builds (`--build-from-source`) have been temporarily removed while the build-path and flag resolution is reworked.
- **Rust & LLVM:**  
  - Installing Rust and LLVM via Sapphire now works end-to-end.  
  - Mach-O/Dylib patching on macOS is largely functional.  
  - Known Issue: `rust-objcopy` (from the Rust cellar) cannot locate LLVM in its expected path. This does not prevent compilation or `rust-analyzer` functionality. A fix is planned once the bottle-installation logic is updated.
- **Cask Functionality:**  
  - `search` and `info` commands are functional.  
  - `install` and `uninstall` remain unimplemented but will be supported soon.
- **Core Commands:**  
  - `update`, `search`, `info`, `install` (formulae), and `uninstall` (formulae) are implemented but may contain bugs.
- **Stability:**  
  - No system-breaking issues have been observed after reinstalling all previously installed formulae.  

## Roadmap

1. **Finish bottle installation logic** to resolve the `rust-objcopy` issue.
2. **Implement Cask install/uninstall** (substantially less complex than formula logic).
3. **Reintroduce source builds** using Homebrew v2 JSON API (`info` route) for build paths and compiler flags. (Core per-build-system code is ready.)
4. **Upgrade feature** to allow upgrading installed formulae and casks.
5. **Cleanup feature** to remove unused downloads, old versions, and leftover files.
6. **Reinstall feature** to reinstall existing formulae or casks easily.
7. **Isolate Sapphire directory** under `/opt/sapphire` instead of Homebrew’s prefix, enabling independent development and testing.
8. **Add `sapphire init`** setup command to bootstrap Sapphire in one step (similar to `brew install --prefix`).

## Upcoming Usage Examples

```bash
# Update package metadata
sapphire update

# Search for formulae
sapphire search <query>

# Install multiple bottles concurrently
sapphire install <formula1> <formula2> <formula3>

# Uninstall multiple bottles concurrently
sapphire uninstall <formula1> <formula2>

# Temporarily removed build-from-source option (coming back soon!)
# sapphire install --build-from-source <formula_name>
```

## Building Sapphire

### Prerequisites

- Rust toolchain (stable recommended)  
- Standard build tools: make, C/C++ compiler (Clang or GCC)  
- CMake, Ninja (often needed as build dependencies)  
- pkg-config

### Build Steps

```bash
git clone <repository_url>
cd sapphire
cargo build --release
```

The `sapphire` binary will be at `target/release/sapphire`. Add it to your `PATH` for testing.

---

### Contributing

Contributions are welcome! Focus areas:

- Stabilizing core features  
- Improving error handling  
- Adding comprehensive tests  
- Refining CLI UX/UI

Feel free to open issues or pull requests.

## License

Sapphire is licensed under the BSD 3-Clause License. See [LICENSE.md](LICENSE.md) for details.

It incorporates concepts inspired by Homebrew (BSD 2-Clause License). See [licenses/LICENSE-Homebrew.md](licenses/LICENSE-Homebrew.md).
