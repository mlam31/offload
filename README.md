# offload

A small terminal app that offloads Sony A6700 media from an SD card to your SSD,
sorted by capture date, with content-based dedup.

```
/Volumes/MySSD/
└── 14-11-2026/
    ├── Clips/
    └── Photos/
        ├── JPEG/
        └── ARW/
```

## Features

- **Interactive TUI** — browse your drives with the arrow keys (`↑↓` move,
  `→` expand, `←` back, `Enter` on an expanded folder to pick it as
  destination), confirm, and watch the import progress.
- **Capture-date sorting** — photos use EXIF `DateTimeOriginal`, clips use the
  file modification time (Sony writes the recording time there). Falls back
  gracefully when EXIF is missing.
- **No duplicates, ever** — every file is fingerprinted (SHA-256). Files
  already imported are warned about and skipped, even if their filename
  changed. Two identical files on the card become one import.
- **Copy-only** — the app has no delete or move code path. Your SD card is
  never modified. Clear it by formatting in camera.
- **Optional rename on import** — `DSC01234.ARW` → `2026-11-14_007.arw`
  (date + sequence per capture date and type), ordered by the camera's own
  numbering. The original name is recorded in the import index.
- **Safe defaults** — free-space check before starting, size-verified copies,
  `q` cancels mid-import while keeping what already landed, and every copied
  file is indexed immediately so an interrupted run never creates duplicates
  when re-run.

## Usage

```sh
# Interactive (recommended)
offload

# Headless
offload --src /Volumes/SD_CARD --dest /Volumes/MySSD
offload --src ... --dest ... --dry-run      # preview only
offload --src ... --dest ... --no-rename    # keep camera filenames
offload --src ... --dest ... --force        # re-import known content
```

State lives in `~/Library/Application Support/offload/`:
`index.json` (imported-content hashes) and `config.json` (last destination).

## Download

Prebuilt binaries are on the [Releases](https://github.com/mlam31/offload/releases) page:

| File | Platform |
|---|---|
| `offload-vX.Y.Z-aarch64-apple-darwin.tar.gz` | macOS — Apple Silicon (M1/M2/M3/M4) |
| `offload-vX.Y.Z-x86_64-apple-darwin.tar.gz` | macOS — Intel |
| `offload-vX.Y.Z-x86_64-pc-windows-gnu.zip` | Windows 64-bit |
| `offload-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` | Linux x86_64 (static, works on any distro) |

Unpack, and run the `offload` binary inside (`offload.exe` on Windows).
Each archive also contains this README.

## Build

```sh
cargo build --release
# binary at target/release/offload — copy wherever you like, e.g.
cp target/release/offload /usr/local/bin/
```

## Platform notes

- **macOS (best supported)**: SD-card auto-detection and the eject feature use
  macOS-specific tools (`diskutil`, `lsof`).
- **Windows / Linux**: everything else works the same — run headless with
  `--src`/`--dest`, or browse the TUI tree from the filesystem root / drive
  letters. Auto-detect looks for a volume layout only on macOS; elsewhere just
  point `--src` at the card's mount or pick it in the tree.

## Supported formats

`.JPG` `.JPEG` → `Photos/JPEG/` · `.ARW` → `Photos/ARW/` · `.MP4` `.MOV` → `Clips/`
