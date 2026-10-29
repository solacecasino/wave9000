# wave9000's ideals and how it's built

The same ideals as Folio, Leaf and splice9000, its sibling. For the full how
and why, see [ARCHITECTURE.md](../ARCHITECTURE.md).

## Core ideals

1. **Start bare.** A fresh install opens to tracks and a mixer and nothing
   else: no browser of paid loops, no start screen, no account, no template
   store. Every extra feature arrives only when the user installs it.
2. **Endlessly customizable, through files.** There's no settings window.
   Behaviour, shortcuts and looks live in plain text files
   (`settings.toml`, `keys.toml`, `layout.toml`) in one folder.
   - The app reloads them as soon as they're saved, with no restart.
   - Copying the folder moves your whole setup.
   - Every file ships with commented examples.
3. **Absolute backward compatibility.** Nothing that worked before may stop
   working: projects, configs, key bindings, the adapter protocol, file
   associations. Old names keep working alongside new ones.
4. **The user owns their data.** Projects are readable JSON, takes are
   plain WAV files beside them. There's no cloud, telemetry or account, and
   the app itself never goes online.
5. **Lean and honest about cost.** Memory, download size, idle CPU and
   playing CPU are measured, not guessed, and every release reports real
   numbers, including what didn't work. Idle means idle: the sound device
   is opened on first play and paused whenever nothing plays.
6. **Never crash on the user's account.** Bad config falls back to
   defaults and shows the problem on screen. ffmpeg errors go to the Log.
   Crashes leave a report for next time, and the project is recovered.
7. **The sound never waits for the screen.** The audio callback never
   locks, allocates, decodes or reads a file.
8. **A sibling, not a copy.** wave9000 and splice9000 share a look, a
   layout system, key conventions, the adapter host and their file habits,
   and wave9000 opens splice9000's projects. Each stays its own app.

## Architecture

- **One small core.** Tracks, mixer, recording, layout and export in one
  binary, plus Folio's CPU-drawn window. No codecs are linked in: files are
  read and written by the system's ffmpeg, run as a separate program.
  WAV files at the device's rate, including takes, are read without it.
- **Decode once, then only add.** Each file is decoded once into memory,
  stereo, at the sound device's own rate, so playing is only multiplying
  and adding. The mixer gets a frozen copy of the project whenever it
  changes, through a slot it never waits on.
- **The maths is cheap.** Each track's clips are sorted by start, with the
  running furthest end kept beside them, so the clips under any moment are
  a binary search away even when clips overlap. Waveforms are drawn from a
  pyramid of peaks, so any zoom reads a few values per point.
- **Everything is a panel, and panels go anywhere** (from splice9000).
- **Adapters for everything else** (from splice9000).
- **Everything is a named command.** One table drives keys, menus, buttons
  and the Help panel.
- **The CPU draws only what changed** (from Folio).
