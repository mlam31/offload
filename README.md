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

## Build

```sh
cargo build --release
# binary at target/release/offload — copy wherever you like, e.g.
cp target/release/offload /usr/local/bin/
```

## Supported formats

`.JPG` `.JPEG` → `Photos/JPEG/` · `.ARW` → `Photos/ARW/` · `.MP4` `.MOV` → `Clips/`
