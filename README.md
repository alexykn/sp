# sps

> [!WARNING]
> **ALPHA SOFTWARE**
> sps is experimental, under heavy development, and may be unstable. Use at your own risk!
>
> **BREAKING CHANGE:**
> Reverted back to using shared $HOMEBREW_PREFIX (/opt/homebrew/ fallback) for the root because a dedicated prefix breaks some formulae and bottle installs.
> Run `sps init` to bootstrap your environment (unchanged).
>
> Uninstalling a cask with brew then reinstalling it with sps will have it installed with slightly different paths, your user settings etc. will not be migrated automatically.

sps is a next‑generation, Rust‑powered package manager inspired by Homebrew. It installs and manages:

- **Formulae:** command‑line tools, libraries, and languages
- **Casks:** desktop applications and related artifacts on macOS

> _ARM only for now, might add x86 support eventually_

---

## ⚙️ Project Structure

- **sps‑core** Core library: fetching, dependency resolution, archive extraction, artifact handling (apps, binaries, pkg installers, fonts, plugins, zap/preflight/uninstall stanzas, etc.)

- **sps‑cli** Command‑line interface: `sps` executable wrapping the core library.

---

## 🚧 Current Status

- Bottle installation and uninstallation
- Cask installation and uninstallation
- Reinstall command for reinstalls
- Upgrade command for updates (Casks deactivated at the moment)
- Parallel downloads and installs for speed
- Automatic dependency resolution and installation
- Building Formulae from source (very early impl)

---

## 🚀 Roadmap

- **Cleanup** old downloads, versions, caches
- **Prefix isolation:** support `/opt/sps` as standalone layout
- **`sps init`** helper to bootstrap your environment
- **Ongoing** Bug fixes and stability improvements

---

## Trying it out:

```bash
cargo install sps
```
> due too the amount of work keeping the crates up to date with every change would entail the crates.io published version will only be updated after major changes or fixes (if there are none expect once a week)
---

<img width="932" alt="Screenshot 2025-04-28 at 23 43 35" src="https://github.com/user-attachments/assets/473347a7-210c-41a3-bb49-e233cfa43c46" />

---

## 📦 Usage

```sh
# Print help
sps --help

# Update metadata
sps update

# List all installed packages
sps list

# List only installed formulae
sps list --formula

# List only installed casks
sps list --cask

# Search for packages
sps search <formula/cask>

# Get package info
sps info <formula/cask>

# Install bottles or casks
sps install <formula/cask>

# Build and install a formula from source
sps install --build-from-source <formula>

# Uninstall
sps uninstall <formula/cask>

# Reinstall
sps reinstall <formula/cask>

#Upgrade
sps upgrade <formula/cask> or --all

# (coming soon)
sps cleanup
sps init
```

-----

## 🏗️ Building from Source

**Prerequisites:** Rust toolchain (stable).

```sh
git clone <repo-url>
cd sps
cargo build --release
```

The `sps` binary will be at `target/release/sps`. Add it to your `PATH`.


-----

## 📥 Using the Latest Nightly Build

You can download the latest nightly build from [`actions/workflows/rust.yml`](../../actions/workflows/rust.yml) inside this repository (select a successful build and scroll down to `Artifacts`).

Before running the downloaded binary, remove the quarantine attribute:

```sh
xattr -d com.apple.quarantine ./sps
```

Then, you can run the binary directly:

```sh
./sps --help
```


-----

## 🤝 Contributing

sps lives and grows by your feedback and code\! We’re particularly looking for:

  - Testing and bug reports for Cask & Bottle installation + `--build-from-source`
  - Test coverage for core and cask modules
  - CLI UI/UX improvements
  - See [CONTRIBUTING.md](CONTRIBUTING.md)

Feel free to open issues or PRs. Every contribution helps\!

-----

## 📄 License

  - **sps:** BSD‑3‑Clause - see [LICENSE.md](LICENSE.md)
  - Inspired by Homebrew BSD‑2‑Clause — see [NOTICE.md](NOTICE.md)

-----

> *Alpha software. No guarantees. Use responsibly.*
