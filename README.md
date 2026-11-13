# mirrorscope

A native, Rust + [Slint](https://slint.dev) GUI replacement for `reflector` on Arch Linux.
No Python at runtime; reflector's logic is reimplemented natively.

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

## Install (AppImage, Arch Linux x86_64)

Download `mirrorscope-<version>-x86_64.AppImage` and its `.sha256` from the
[releases page](https://github.com/legendarylolo318-cloud/mirrorscope/releases), then:

```bash
sha256sum -c mirrorscope-0.1.0-x86_64.AppImage.sha256
chmod +x mirrorscope-0.1.0-x86_64.AppImage
./mirrorscope-0.1.0-x86_64.AppImage                  # GUI
./mirrorscope-0.1.0-x86_64.AppImage --cli -c de -n 5  # CLI, prints to stdout
```

Runtime requirements (all standard on an Arch desktop): `fuse3` (or run with
`--appimage-extract-and-run`), `fontconfig`, `polkit` with a running
authentication agent, and Wayland or X11.

### One-time: install the privileged helper and polkit policy

Writing `/etc/pacman.d/mirrorlist` and installing the timer need root. The GUI never
runs as root; it calls a small validating helper through `pkexec`. Because root cannot
read an AppImage's FUSE mount, **the helper and its polkit policy must be installed
system-wide** before writes work. Either click **Timer / Setup → Install helper & polkit
policy…** in the app, or run:

```bash
./mirrorscope-0.1.0-x86_64.AppImage --install-helper
```

This asks for your password once (polkit `auth_admin`) and installs:

| File | Purpose |
|---|---|
| `/usr/lib/mirrorscope/mirrorscope-helper` | validating, root-only writer |
| `/usr/lib/mirrorscope/mirrorscope` | root-owned copy of the app, used by the systemd timer |
| `/usr/share/polkit-1/actions/io.github.mirrorscope.policy` | polkit action (`auth_admin`) |

Equivalent manual install from an extracted AppImage:

```bash
./mirrorscope-0.1.0-x86_64.AppImage --appimage-extract
sudo install -Dm755 squashfs-root/usr/lib/mirrorscope/mirrorscope-helper /usr/lib/mirrorscope/mirrorscope-helper
sudo install -Dm755 squashfs-root/usr/bin/mirrorscope /usr/lib/mirrorscope/mirrorscope
sudo install -Dm644 squashfs-root/usr/share/polkit-1/actions/io.github.mirrorscope.policy \
                    /usr/share/polkit-1/actions/io.github.mirrorscope.policy
```

Uninstall: `sudo rm -r /usr/lib/mirrorscope /usr/share/polkit-1/actions/io.github.mirrorscope.policy`
(remove the timer from the app first if you installed it). Re-run `--install-helper`
after upgrading the AppImage so the timer uses the new version.

## CLI

```
mirrorscope --cli --country Germany,France --protocol https --age 12 \
            --latest 100 --sort rate --number 10 --save /etc/pacman.d/mirrorlist
mirrorscope --cli --preset weekly --save /etc/pacman.d/mirrorlist
mirrorscope --cli --list-countries
```

Without `--save` the list goes to stdout. Saving to `/etc/pacman.d/mirrorlist` uses the helper
(via `pkexec`, or directly when already root, e.g. from the timer).

## Security model

- `mirrorscope-helper` is the only code that runs as root. It accepts exactly four
  commands (`write`, `restore <backup>`, `install-timer <preset> <config>`, `remove-timer`)
  and no caller-controlled paths.
- Every line written must be empty, a printable-ASCII comment, or
  `Server = http(s)|rsync://host[:port]/path/$repo/os/$arch` (no userinfo, `..`, `%`,
  query, IPv6 literal, control characters, NUL or non-ASCII). Input is capped at 256 KiB.
- Writes go only to `/etc/pacman.d/mirrorlist` and `/etc/pacman.d/mirrorlist.bak.*`:
  the current file is backed up first, then replaced atomically (O_EXCL|O_NOFOLLOW temp
  file, fsync, rename, fsync dir). Symlinks, FIFOs and directories are refused.
- Timer units are generated from a validated preset name and config path and run a
  root-owned binary with systemd sandboxing (`ProtectSystem=strict`, `ReadWritePaths=/etc/pacman.d`, ...).
- The polkit action uses `auth_admin` for every case.

## Build

Requires Rust (stable) and, on Arch, `base-devel`, `fontconfig`, `libxkbcommon`, `wayland`.

```bash
cargo build --release
cargo test --workspace
./build-appimage.sh      # -> dist/mirrorscope-<version>-x86_64.AppImage (+ .sha256)
```

`build-appimage.sh` uses `$APPIMAGETOOL`, an `appimagetool` on `PATH`, or downloads the
pinned appimagetool 1.9.1 release and verifies its SHA-256. Pushing a `v*` tag builds the
AppImage on `archlinux:latest` in GitHub Actions and attaches it to the release.

Manual install from source (instead of the AppImage):

```bash
sudo install -Dm755 target/release/mirrorscope /usr/bin/mirrorscope
sudo install -Dm755 target/release/mirrorscope /usr/lib/mirrorscope/mirrorscope
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

## Known limitations

- x86_64 Arch Linux only. The AppImage relies on the system's glibc/fontconfig/polkit.
- The speed test downloads `core.db` (~130 KB) once per mirror, like reflector. On
  high-latency links the result is dominated by round-trip time, not bandwidth.
- rsync mirrors cannot be speed-tested; with `--sort rate` they are dropped.
- The timer runs as root and reads your user config file (read-only); its output still
  passes through the helper's validation.
- The installed helper/timer binary is not updated automatically when you replace the
  AppImage; re-run `--install-helper`.

## License

MIT
