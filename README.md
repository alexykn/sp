# sph

> [!WARNING]
> **ALPHA SOFTWARE**
>sp his experimental, under heavy development, and may be unstable. Use at your own risk!
>
> Uninstalling a cask with brew then reinstalling it with sph will have it installed with slightly different paths, your user settings etc. will not be migrated automatically.

sph his a next‑generation, Rust‑powered package manager inspired by Homebrew. It installs and manages:

- **Formulae:** command‑line tools, libraries, and languages  
- **Casks:** desktop applications and related artifacts on macOS

> _ARM only for now, might add x86 support eventually_

---

## ⚙️ Project Structure

- **sph‑core** Core library: fetching, dependency resolution, archive extraction, artifact handling (apps, binaries, pkg installers, fonts, plugins, zap/preflight/uninstall stanzas, etc.)

- **sph‑cli** Command‑line interface: `sph` executable wrapping the core library.

---

## 🚧 Current Status

- Bottle installation and uninstallation  
- Cask installation and uninstallation
- Reinstall command for reinstalls
- Upgrade command for updates (very careful. I ran into no system breakers, my Perl install got nuked though)
- Parallel downloads and installs for speed  
- Automatic dependency resolution and installation
- Building Formulae from source (very early impl)

---

## 🚀 Roadmap

- **Cleanup** old downloads, versions, caches  
- **Prefix isolation:** support `/opt/sph` as standalone layout  
- **`sph init`** helper to bootstrap your environment
- **Ongoing** Bug fixes and stability improvements

---

<img width="856" alt="Screenshot 2025-04-26 at 22 09 41" src="https://github.com/user-attachments/assets/bd4a39ed-d4b3-4d19-9b1c-2edcba5f472d" />

> I know this does not follow one defined style yet. Still thinking about how I actually want it to look so... we'll get there

---

## 📦 Usage

```sh
# Print help
sph --help

# Update metadata
sph update

# Search for packages
sph search <formula/cask>

# Get package info
sph info <formula/cask>

# Install bottles or casks
sph install <formula/cask>

# Build and install a formula from source
sph install --build-from-source <formula>

# Uninstall
sph uninstall <formula/cask>

# Reinstall
sph reinstall <formula/cask>

#Upgrade
sph upgrade <formula/cask> or --all

# (coming soon)
sph cleanup
sph init
```

-----

## 🏗️ Building from Source

**Prerequisites:** Rust toolchain (stable).

```sh
git clone <repo-url>
cd sph
cargo build --release
```

The `sph` binary will be at `target/release/sph`. Add it to your `PATH`.


-----

## 📥 Using the Latest Nightly Build

You can download the latest nightly build from [`actions/workflows/rust.yml`](../../actions/workflows/rust.yml) inside this repository (select a successful build and scroll down to `Artifacts`).

Before running the downloaded binary, remove the quarantine attribute:

```sh
xattr -d com.apple.quarantine ./sph
```

Then, you can run the binary directly:

```sh
./sph --help
```


-----

## 🤝 Contributing

sph lives and grows by your feedback and code\! We’re particularly looking for:

  - Testing and bug reports for Cask & Bottle installation + `--build-from-source`
  - Test coverage for core and cask modules
  - CLI UI/UX improvements
  - See [CONTRIBUTING.md](CONTRIBUTING.md)

Feel free to open issues or PRs. Every contribution helps\!

-----

## 📄 License

  - **sph:** BSD‑3‑Clause - see [LICENSE.md](LICENSE.md)
  - Inspired by Homebrew BSD‑2‑Clause — see [NOTICE.md](NOTICE.md)

-----

> *Alpha software. No guarantees. Use responsibly.*
