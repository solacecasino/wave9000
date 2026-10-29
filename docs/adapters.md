# wave9000 adapters

wave9000 ships bare. An **adapter** is an add-on you choose to have, like
foobar2000's components: a folder with an `adapter.toml` and, usually, a
program in any language. wave9000 starts the program when it's needed and
talks to it with JSON messages, one per line, over stdin and stdout. An
adapter can crash without taking wave9000 down.

The host is splice9000's, so an adapter written for one reads naturally
for the other. What an adapter sees and changes is different: here it's
tracks of clips instead of one timeline.

An adapter can add:

- **commands**, in any menu and bindable to keys;
- **panels** you place in the layout like any other;
- **export formats**, which need no program at all: an adapter can be only
  an `adapter.toml` of presets.

0.0.1 comes with no adapters, and there's no catalog yet.

## Installing

**Drop an adapter's zip or folder on wave9000's window** (or **Adapters →
Install Adapter…**). wave9000 shows what it adds and what it may do, and
installs it only when you click **Install**: Return does nothing and Escape
declines, so a stray key press can't install anything. wave9000 itself
never goes online.

Installed adapters live in `adapters/` in the settings folder. The first
time an adapter you copied in by hand runs, wave9000 asks before starting
it. Installing through the prompt counts as allowing it. Answers are kept
in `approvals.json` until the adapter asks for more.

**Remove** one in the Adapters panel: it goes to the Trash.

## `adapter.toml`

```toml
name = "Loudness"               # shown in menus and prompts; the adapter's identity
version = "0.1.0"
author = "you"
license = "MIT"
description = "One sentence about what it does."
homepage = "https://github.com/you/loudness"
requires = "0.0.1"              # optional: the oldest wave9000 it works with

run = ["python3", "main.py"]    # program + arguments, run from this folder
permissions = ["edit.read", "edit.change"]
start = "command"               # "command" (default) or "launch"
events = ["changed"]            # events to receive while running

[[commands]]
id = "measure"                  # sent to you when chosen
label = "Measure Loudness"
keys = "cmd+shift+l"            # optional default shortcut; keys.toml can change it
menu = "track"                  # optional: "file", "edit", "track", "view" or "playback"; else Adapters

[[panels]]
id = "meter"                    # filled with panel.set
title = "Loudness"

[[exports]]                     # no program needed for these
id = "spotify"
label = "Spotify (-14 LUFS, MP3)"
extension = "mp3"
audio_filter = "loudnorm=I=-14:TP=-1"  # an ffmpeg filter on the finished mix
audio = ["-c:a", "libmp3lame", "-b:a", "320k"]  # ffmpeg encoder arguments; left out: the extension's own
```

- `run` is started in the adapter's folder. A program given as a path
  (`./loudness`) is found there, and on Windows `.exe` is added if that's
  what's there, so one `adapter.toml` works everywhere.
- wave9000 sets `WAVE9000_CONFIG_DIR`, `WAVE9000_FFMPEG` and
  `WAVE9000_FFPROBE`, so adapters can keep their own files and use the same
  ffmpeg without bringing one.
- Whatever the program prints to stderr shows in the Log panel.
- Commands can be bound in keys.toml: `"cmd+k" = 'adapter "Loudness" measure'`.
  Export formats too: `"cmd+shift+e" = 'export "Loudness" spotify'`.
- Panels go in layout.toml as `panel = "adapter:<name>/<panel id>"`, or
  with the panel buttons in the Adapters panel.
- Unknown keys are ignored, so newer adapter.toml files work in older
  wave9000s as far as they can. When that isn't far enough, set `requires`.

### Permissions

| Permission | Allows |
|---|---|
| `edit.read` | `edit.get`, and events |
| `edit.change` | `edit.set`, `edit.import`, `playhead.set` |
| `network` | Nothing through wave9000; it tells people the adapter goes online |

An adapter is a normal program running as you, so permissions control what
it can do **through wave9000**, and `network` is an honest declaration, not
a sandbox. Only install adapters you trust.

## Messages

Every message is one JSON object on one line, in UTF-8. A **notification**
has `method` and `params` and gets no reply. A **request** also has an `id`;
the reply has the same `id` and either `result` or `error` (a string). Only
adapters send requests.

### wave9000 → adapter

| Method | Params | When |
|---|---|---|
| `initialize` | `wave9000` (version), `protocol` (`1`), `permissions`, `ffmpeg`, `ffprobe` (paths, or null) | Right after starting |
| `command` | `command`: the command's `id` | One of its commands was chosen |
| `event` | `kind` plus fields below | Something happened, if listed in `events` (needs `edit.read`) |
| `shutdown` | | wave9000 is quitting or the adapter is being removed; exit soon |

| Event `kind` | Fields |
|---|---|
| `changed` | `duration`, `tracks` (how many): at most once a frame while editing |
| `media` | `paths`: the media list, when files are added or a project opened |
| `exported` | `path` |
| `saved` | `path`: a project file was saved |

### Adapter → wave9000

| Method | Params | Result | Permission |
|---|---|---|---|
| `edit.get` | | The project (below) | `edit.read` |
| `edit.set` | `tracks` (as in the project), optional `master_db` | `null`. Replaces the tracks as one undo step. Each clip is checked against its media file | `edit.change` |
| `edit.import` | `paths`, `place` (bool) | `null`. Adds sound files; with `place`, each on a new track at the playhead | `edit.change` |
| `playhead.set` | `time` (seconds) | The playhead | `edit.change` |
| `playback.set` | `playing` (bool) | Whether it's playing | none |
| `panel.set` | `id`, `lines` | `null`. Each line is a string, or `{ "text", "command" }` for a button running one of your commands | none |
| `notify` | `text` | `null`. A short message at the bottom of the window | none |
| `log` | `message` | Written to the Log panel | none |
| `ui.confirm` | `title`, `message` | `true` or `false` | none |
| `ui.prompt` | `title`, `message`, `default` | The text typed, or `null` | none |
| `ui.save_dialog` | `title`, `file_name`, `extension` | The chosen path, or `null` | none |
| `ui.open_dialog` | `title`, `extensions` | The chosen path, or `null` | none |

### The project

```json
{
  "duration": 16.0,
  "playhead": 3.0,
  "playing": false,
  "rate": 48000,
  "selected": { "track": 0, "clip": 1 },
  "file": "/Users/me/song.wave",
  "master_db": 0.0,
  "media": [
    { "path": "/Users/me/kick.wav", "duration": 8.0, "rate": 48000, "channels": 2 }
  ],
  "tracks": [
    {
      "name": "Kick", "gain_db": -2.0, "pan": 0.0, "mute": false, "solo": false, "armed": false,
      "clips": [{ "media": 0, "at": 0.0, "src_in": 0.0, "src_out": 8.0, "gain_db": 0.0 }]
    }
  ]
}
```

Times are seconds. `at` is where a clip starts on its track; `src_in` and
`src_out` are times in its media file. `pan` is -1 (left) to 1 (right).
Gains are dB; -90 or below is silence. `rate` is the sound device's, which
every file is played at.

## Versions

`initialize` says which protocol version wave9000 speaks. New versions only
add things, so an adapter written for an older version keeps working.

- **1** (wave9000 0.0.1): commands, panels, export formats, the project,
  events and prompts.
