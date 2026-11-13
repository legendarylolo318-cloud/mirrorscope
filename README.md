# mirrorscope

A native, Rust + [Slint](https://slint.dev) GUI replacement for `reflector` on Arch Linux.
No Python at runtime; reflector's logic is reimplemented natively.

![screenshot placeholder](docs/screenshot.png)

## Features

- Fetches <https://archlinux.org/mirrors/status/json/> (reqwest + rustls, no OpenSSL)
- Filters: country (multi-select with search and counts), protocol (https default), max age,
  completion %, IPv4/IPv6, include/exclude regex on URL
- Sorting by score, delay, last sync, country, URL, completion and **measured download rate**
- Concurrent speed test of `core/os/x86_64/core.db` (configurable concurrency/timeout, cancellable, live per-row status)
- Sortable table with per-mirror include/exclude checkboxes and a top-N selector
- Live preview of the exact mirrorlist (reflector-style header)
- Safe writes: the GUI never runs as root. `mirrorscope-helper` runs through `pkexec`, validates every line,
  backs up to `/etc/pacman.d/mirrorlist.bak.<timestamp>` and replaces the file atomically (temp file, fsync, rename)
- Backups tab: list, diff against current, restore
- Presets and last-used settings in `~/.config/mirrorscope/config.toml`
- Weekly systemd timer generator (review the units, install via the helper)
- Headless CLI: `mirrorscope --cli ...`

## CLI

```
mirrorscope --cli --country Germany,France --protocol https --age 12 \
            --latest 100 --sort rate --number 10 --save /etc/pacman.d/mirrorlist
mirrorscope --cli --preset weekly --save /etc/pacman.d/mirrorlist
mirrorscope --cli --list-countries
```

Without `--save` the list goes to stdout. Saving to `/etc/pacman.d/mirrorlist` uses the helper
(via `pkexec`, or directly when already root, e.g. from the timer).

## Build

Requires Rust (stable) and, on Arch, `fontconfig`, `libxkbcommon`, `wayland`, and `base-devel`.

```bash
cargo build --release
cargo test --workspace
```

Manual install:

```bash
sudo install -Dm755 target/release/mirrorscope /usr/bin/mirrorscope
sudo install -Dm755 target/release/mirrorscope-helper /usr/lib/mirrorscope/mirrorscope-helper
sudo install -Dm644 packaging/polkit/io.github.mirrorscope.policy /usr/share/polkit-1/actions/io.github.mirrorscope.policy
sudo install -Dm644 packaging/mirrorscope.desktop /usr/share/applications/mirrorscope.desktop
sudo install -Dm644 packaging/icons/mirrorscope.svg /usr/share/icons/hicolor/scalable/apps/mirrorscope.svg
sudo install -Dm644 packaging/mirrorscope.metainfo.xml /usr/share/metainfo/io.github.mirrorscope.Mirrorscope.metainfo.xml
```

During development the GUI finds `mirrorscope-helper` next to its own binary (override with `MIRRORSCOPE_HELPER`).

## Layout

```
crates/core    fetch, filter, sort, select, speedtest, mirrorlist format, config, backups, unit files
crates/helper  privileged helper (validation + atomic writes)
crates/cli     headless mode
crates/gui     Slint UI; the `mirrorscope` binary (dispatches to the CLI on --cli)
```

## Compared to reflector

| | reflector | mirrorscope |
|---|---|---|
| Runtime | Python | native Rust binary |
| Interface | CLI | GUI + CLI |
| Speed test | `--sort rate` | concurrent, live progress, cancellable |
| Privilege | run whole tool as root | GUI unprivileged, small validating helper via polkit |
| Backups | none | automatic, diff and restore |
| Scheduling | separate systemd unit from package | generated + reviewed in app |
| Missing | `--fastest`, `--connection-timeout`, `--threads`, `--url`, ... | not all flags implemented |

## License

MIT
