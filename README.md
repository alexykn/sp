# spm

> [!WARNING]
> **ALPHA SOFTWARE**
> spm is experimental, under heavy development, and may be unstable. Use at your own risk!
>
> Uninstalling a cask with brew then reinstalling it with spm will have it installed with slightly different paths, your user settings etc. will not be migrated automatically.

spm is a next‑generation, Rust‑powered package manager inspired by Homebrew. It installs and manages:

- **Formulae:** command‑line tools, libraries, and languages  
- **Casks:** desktop applications and related artifacts on macOS

> _ARM only for now, might add x86 support eventually_

---

## ⚙️ Project Structure

- **spm‑core** Core library: fetching, dependency resolution, archive extraction, artifact handling (apps, binaries, pkg installers, fonts, plugins, zap/preflight/uninstall stanzas, etc.)

- **spm‑cli** Command‑line interface: `spm` executable wrapping the core library.

---

## 🚧 Current Status

- Bottle installation and uninstallation  
- Cask installation and uninstallation
- Parallel downloads and installs for speed  
- Automatic dependency resolution and installation
- Building Formulae from source (very early impl)

---

## 🚀 Roadmap

1. **Upgrade** command to update installed packages  
2. **Cleanup** old downloads, versions, caches  
3. **Reinstall** command for quick re‑pours  
4. **Prefix isolation:** support `/opt/spm` as standalone layout  
5. **`spm init`** helper to bootstrap your environment
6. **Ongoing** Bug fixes and stability improvements

---

<img width="856" alt="Screenshot 2025-04-26 at 14 04 22" src="https://github.com/user-attachments/assets/df406637-f7a9-4ff6-b61f-e7e15ce674d8" />

---

## 📦 Usage

```sh
# Print help
spm --help

# Update metadata
spm update

# Search for packages
spm search <formula/cask>

# Get package info
spm info <formula/cask>

# Install bottles or casks
spm install <formula/cask>

# Build and install a formula from source
spm install --build-from-source <formula>

# Uninstall
spm uninstall <formula/cask>

# (coming soon)
spm upgrade [--all] <name>
spm cleanup
spm init
```

-----

## 🏗️ Building from Source

**Prerequisites:** Rust toolchain (stable).

```sh
git clone <repo-url>
cd spm
cargo build --release
```

The `spm` binary will be at `target/release/spm`. Add it to your `PATH`.

-----

## 🤝 Contributing

spm lives and grows by your feedback and code\! We’re particularly looking for:

  - Testing and bug reports for Cask & Bottle installation + `--build-from-source`
  - Test coverage for core and cask modules
  - CLI UI/UX improvements
  - See [CONTRIBUTING.md](CONTRIBUTING.md)

Feel free to open issues or PRs. Every contribution helps\!

-----

## 📄 License

  - **spm:** BSD‑3‑Clause - see [LICENSE.md](LICENSE.md)
  - Inspired by Homebrew BSD‑2‑Clause — see [NOTICE.md](NOTICE.md)

-----

> *Alpha software. No guarantees. Use responsibly.*
