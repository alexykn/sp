# sp

> [!WARNING]
> **ALPHA SOFTWARE**
> sp is experimental, under heavy development, and may be unstable. Use at your own risk!
>
> Uninstalling a cask with brew then reinstalling it with sp will have it installed with slightly different paths, your user settings etc. will not be migrated automatically.

sp is a next‑generation, Rust‑powered package manager inspired by Homebrew. It installs and manages:

- **Formulae:** command‑line tools, libraries, and languages  
- **Casks:** desktop applications and related artifacts on macOS

> _ARM only for now, might add x86 support eventually_

---

## ⚙️ Project Structure

- **sp‑core** Core library: fetching, dependency resolution, archive extraction, artifact handling (apps, binaries, pkg installers, fonts, plugins, zap/preflight/uninstall stanzas, etc.)

- **sp‑cli** Command‑line interface: `sp` executable wrapping the core library.

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
4. **Prefix isolation:** support `/opt/sp` as standalone layout  
5. **`sp init`** helper to bootstrap your environment
6. **Ongoing** Bug fixes and stability improvements

---

<img width="856" alt="Screenshot 2025-04-26 at 22 09 41" src="https://github.com/user-attachments/assets/bd4a39ed-d4b3-4d19-9b1c-2edcba5f472d" />

> I know this does not follow one defined style yet. Still thinking about how I actually want it to look so... we'll get there

---

## 📦 Usage

```sh
# Print help
sp --help

# Update metadata
sp update

# Search for packages
sp search <formula/cask>

# Get package info
sp info <formula/cask>

# Install bottles or casks
sp install <formula/cask>

# Build and install a formula from source
sp install --build-from-source <formula>

# Uninstall
sp uninstall <formula/cask>

# (coming soon)
sp upgrade [--all] <name>
sp cleanup
sp init
```

-----

## 🏗️ Building from Source

**Prerequisites:** Rust toolchain (stable).

```sh
git clone <repo-url>
cd sp
cargo build --release
```

The `sp` binary will be at `target/release/sp`. Add it to your `PATH`.

-----

## 🤝 Contributing

sp lives and grows by your feedback and code\! We’re particularly looking for:

  - Testing and bug reports for Cask & Bottle installation + `--build-from-source`
  - Test coverage for core and cask modules
  - CLI UI/UX improvements
  - See [CONTRIBUTING.md](CONTRIBUTING.md)

Feel free to open issues or PRs. Every contribution helps\!

-----

## 📄 License

  - **sp:** BSD‑3‑Clause - see [LICENSE.md](LICENSE.md)
  - Inspired by Homebrew BSD‑2‑Clause — see [NOTICE.md](NOTICE.md)

-----

> *Alpha software. No guarantees. Use responsibly.*
