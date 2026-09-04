# Tarkov.space - Raid Monitor

A lightweight Windows app that watches your **Escape from Tarkov** logs in real time
and tells you, by server **IP**, whether you re-entered the **same raid** — the classic
"I died as PMC, then ran in as SCAV: is it the same match?" check.

Everything is automatic: no buttons to press while you play.

## Download

**➡ Get the latest `.exe`: [Releases › latest](https://github.com/andre-lobo/Tarkov.space_RaidMonitor/releases/latest)**

Download `Tarkov.space_RaidMonitor.exe` and run it — no install, no dependencies
(single self-contained ~3 MB executable, works on any Windows 10/11).

## Features

- **Live state** (from the game logs): `GAME CLOSED`, `IN MENU`, `IN RAID`, and the
  key alert — **`SAME RAID`** (green + sound) / **`DIFFERENT RAID`** (red) when you
  enter as **SCAV** into your previous **PMC** raid (compared by server IP).
- **Current / last match** card: IP, location, mode, profile, time.
- **PMC / SCAV** and **game-mode** (PvP / PvE / PvP Season) colored tags.
- **Match history** (last 10, across sessions) in a modal.
- Detects PMC vs SCAV across all three modes (PvE / PvP / PvP Season).
- Shows your PMC nickname when available.
- Minimize to the **system tray** (Show/Hide, Quit; left-click to restore).
- Sound alert with a **Test** button; theme colors `#14161A` / `#C4AB7A`.

## Usage

1. Run the `.exe`.
2. Click **Browse** and select your Tarkov **Logs** folder, e.g.:
   - Steam: `C:\Steam\steamapps\common\Escape from Tarkov\build\Logs`
   - Standalone: `C:\Battlestate Games\Escape from Tarkov\Logs`
3. Leave it open while you play. Detection is fully automatic.

Your folder path is remembered in `%APPDATA%\Raid IP Monitor\config.txt`.

## Build from source

Requires the Rust toolchain (MSVC).

```bash
cargo build --release
```

The binary is produced at `target/release/raid-ip-monitor.exe`.

## Releases

Every push to `main` builds on GitHub Actions and updates the **latest** release
with a fresh `Tarkov.space_RaidMonitor.exe`. Pushing a `v*` tag creates a
versioned release as well.

---

Built with Rust + [egui/eframe](https://github.com/emilk/egui).
