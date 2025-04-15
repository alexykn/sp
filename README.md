**IMPORTANT WARNING: ALPHA SOFTWARE**

**Sapphire is currently in an ALPHA stage of development. It is experimental and potentially unstable. While many features are functional, exercise caution, especially when installing complex or foundational packages. Use at your own discretion and risk!**

* **Potential for Installation Issues:** While many formulae (both from bottles and source) may install correctly, installations involving complex dependencies or foundational tools (like compilers, e.g., LLVM, rustc) have a higher chance of failure or causing conflicts. These failures could potentially leave packages in a broken state or interfere with other system components.
* **Cask Support Limited:** Cask `search` and `info` commands are functional. However, **cask `install` and `uninstall` are NOT implemented or supported.**
* **Experimental Features:** Some core package management functionalities might still contain bugs.
* **Use At Your Own Risk:** It's recommended to test Sapphire on non-critical systems or virtual machines first. Do not rely on it yet for managing essential software. **The developers take NO RESPONSIBILITY for any damage caused by using this software.**

---

## Introduction

Sapphire is an experimental package manager written in Rust, inspired by Homebrew. It aims to provide a way to install and manage command-line software (formulae) and eventually applications (casks) on macOS and potentially, eventually, maybe Linux, leveraging modern tools and technologies.

It directly interacts with the official homebrew repositories for formulae, bottles and casks. Adding custom taps is not yet supported.

The project is split into:

* `sapphire-core`: The underlying library handling fetching, dependency resolution, building, installation, etc.
* `sapphire-cli`: The command-line interface tool that users interact with.

## Current Status (Alpha)

* **Formulae Installation:** Many formulae (bottles & source builds) can be installed successfully. Issues are more likely with complex dependencies or foundational packages (e.g., LLVM, Rust). Simpler tools and libraries (e.g., ncurses) are generally more stable.
* **Cask Functionality:** Cask `search` and `info` work. Cask `install` and `uninstall` are **not implemented**.
* **Core Commands:** `update`, `search`, `info`, `install` (formulae), and `uninstall` (formulae) are implemented but may contain bugs.
* **Stability:** While improving, instability (failed installs, incorrect linking, crashes) are still very likely, particularly with complex packages.

## Features

* Fetches and caches package information.
* Resolves package dependencies, including cycle detection.
* Downloads package artifacts (source archives, pre-compiled bottles).
* Verifies downloads using checksums.
* Builds formulae from source using common build systems (Make, CMake, Meson, Cargo, etc.).
* Installs packages into a managed "Cellar" directory.
* Links installed files into the main prefix, using wrapper scripts for executables.
* Provides build environment sanitization.
* Handles Mach-O binary patching on macOS for correct linking.

## Usage

```bash
# Update local cache of package lists (Required before searching/installing)
sapphire update

# Search for formulae and casks
# (Note: Only formulae can be installed/uninstalled currently)
sapphire search <query>
sapphire search --formula <query>
sapphire search --cask <query>

# Show information about a formula or cask
sapphire info <package_name>
sapphire info --cask <cask_name>

# Install a formula (EXPERIMENTAL - Can fail with complex packages)
sapphire install <formula_name>
# Force building from source (even more experimental)
sapphire install --build-from-source <formula_name>

# Uninstall a formula (EXPERIMENTAL - may not clean up properly)
sapphire uninstall <formula_name>
```

## Building Sapphire

Prerequisites:

- Rust toolchain (stable recommended)

Optional but recommended due too unreliable sapphire install:
- Standard build tools (make, C/C++ compiler like Clang or GCC)
- CMake, Ninja (often needed as build dependencies for formulae)
- pkg-config

Steps:

Clone the repository:
```bash
git clone <repository_url>
cd sapphire
cargo build --release
```

**Find the executable:** The binary will be located at target/release/sapphire. You might want to add this location to your PATH for testing, but do not replace your existing package manager (like Homebrew).

Contributing
Given the early and unstable nature of the project, contributions are welcome but should focus on stabilizing core features, improving error handling, and adding comprehensive tests. Please be aware that major refactoring might occur at any time.

License
This project is licensed under the BSD 3-Clause License. See the LICENSE.md file for details.

It utilizes components or concepts inspired by Homebrew, which is licensed under the BSD 2-Clause License. See licenses/LICENSE-Homebrew.md.
