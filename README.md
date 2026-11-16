# ConfigVault

Versioned, verifiable backups of your configuration files — with dry-run restores, SHA-256 checksums, and an automatic safety backup taken **before** every restore.

**Your files never leave this machine.** No account, no cloud, no telemetry. Archives live in ConfigVault's app-data directory.

## Features

- **Snapshots of any folders/files** — pick dotfiles, `~/.config/…`, project configs, anything readable
- **Exclude patterns** — one per line: `name` (path component), `*.ext`, or `/abs/prefix` (e.g. `node_modules`, `.git`, `*.log`); defaults are prefilled
- **Every file SHA-256 verified on restore** — corrupt archives or half-written files are detected, reported, and never silently trusted
- **Dry-run preview** — see exactly what would be created/overwritten (and how many entries are invalid) before anything is written
- **Pre-restore safety backup** — existing files are copied into an automatic snapshot before the restore overwrites them; restore never deletes anything
- **Export / import** — copy a `.tar.gz` snapshot anywhere, or import one on another machine (import re-validates the manifest)
- **Atomic metadata writes**, no background watching, symlinks are never followed (no loops, no roots escapes)

## Install & run

Requires [Rust](https://rustup.rs), [Node.js](https://nodejs.org) ≥ 20, and pnpm. On Linux the Tauri system dependencies are needed
(`libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev` on Debian/Ubuntu; `webkit2gtk4.1-devel libsoup3-devel` on Fedora).

```sh
pnpm install
pnpm tauri build     # produces .deb/.rpm/.AppImage under src-tauri/target/release/bundle/
```

Development:

```sh
pnpm dev             # frontend only (vite, port 1426)
pnpm tauri dev       # full desktop app with hot reload
pnpm test            # vitest unit tests
cd src-tauri && cargo test   # Rust unit tests (roundtrip, dry-run, pre-restore, traversal safety…)
```

## How it works

- **Archive layout** (`<id>.tar.gz`): `manifest.json` + `data/<source-index>/<relative path>`. The manifest lists every entry with its absolute target path, size and SHA-256. A small `<id>.json` sidecar holds the summary so listing backups never opens the archives.
- **Caps** — 20 000 entries, 4 GiB total, 256 MiB per file, 32 directory levels, symlinks skipped.
- **Restore path safety** — archive manifests are re-validated on every restore: absolute paths only, no `..`, no NUL bytes. A crafted archive cannot overwrite `/etc/passwd` through a traversal entry; invalid entries are reported and skipped, valid ones still restore.
- **Pre-restore backup** — before overwriting anything, ConfigVault snapshots the files that exist (into the same backup store, named "Pre-restore backup …") so you can roll back a bad restore.
- **Restore does not delete** — snapshots are purely additive writes; there is no delete-wins behavior.
- **Exclude semantics** — `name` matches any path component (catches `a/node_modules/b`), `*.ext` matches file names case-insensitively, `/abs/prefix` matches by absolute path prefix. A leading `*` matches everything.

## Privacy & security

- ConfigVault makes **no network connections** of any kind.
- Backup archives are stored under the Tauri app-data directory (`~/.local/share/com.can5.configvault` on Linux, `%APPDATA%`-equivalent on Windows, `~/Library/Application Support` on macOS) in `backups/`.
- Secrets stored in config files (tokens, keys) are only as safe as that directory. Protect your user account; archives are not encrypted at rest.
- Export/import uses a native file picker; the export path must be absolute.
- Deleting a snapshot removes both the archive and its sidecar. The pre-restore backup created at restore time is a normal snapshot and is kept until you delete it.

## Platform notes

- **Linux**: full support. XDG paths for app data.
- **Windows / macOS**: supported. Unix file modes are preserved on Linux/macOS; on Windows, mode bits are ignored on restore.
- Symlinks inside backed-up trees are skipped (by design) — only regular files and directories are snapshotted.

## License

MIT — see [LICENSE](LICENSE).
