# wave9000

A bare, modular audio workstation. Record and arrange sound on tracks, mix
it, export it. Every panel can be moved, added or removed, the way
foobar2000 lets you rearrange its window. It's the sibling of
[splice9000](https://github.com/solacecasino/splice9000), the video editor,
and opens its projects. Its sister is
[grace9000](https://github.com/solacecasino/grace9000), the photo editor.

The whole 9000 series: [text9000](https://github.com/solacecasino/text9000)
(writing), [splice9000](https://github.com/solacecasino/splice9000) (video),
[wave9000](https://github.com/solacecasino/wave9000) (sound),
[grace9000](https://github.com/solacecasino/grace9000) (photos) and
[leaf9000](https://github.com/solacecasino/leaf9000) (comics and books).

![wave9000](docs/screenshot.png)

It's a prototype (0.0.3): tracks, clips, a mixer, recording, export.
Nothing else is in the app.

## Installing

1. Download `wave9000-<version>-macos-universal.zip` from
   [Releases](https://github.com/solacecasino/wave9000/releases) (Apple
   Silicon and Intel Macs, macOS 11 or later).
2. Unzip it and move wave9000.app to Applications.
3. The first time, macOS says it can't check the app: it isn't signed with
   an Apple developer certificate. Open **System Settings → Privacy &
   Security**, scroll down to wave9000 and click **Open Anyway** (on macOS
   14 and earlier, **right-click → Open** does the same). Or run
   `xattr -dr com.apple.quarantine /Applications/wave9000.app`. The first
   time you record, macOS asks to let it use the microphone.
4. Install [ffmpeg](https://ffmpeg.org) to add files other than WAV:
   `brew install ffmpeg`. wave9000 runs it as a separate program for
   reading and writing files, so no codecs are built in. Without it,
   recording, WAV files at your sound device's rate and WAV export still
   work.

**Windows** (x64 and Arm): unzip `wave9000-<version>-windows-<x64|arm64>.zip`
anywhere and run `wave9000.exe`. SmartScreen may ask once, since the app
isn't signed: **More info → Run anyway**. Put ffmpeg on your PATH (for
example `winget install ffmpeg`). If recording gives only silence, allow
apps to use the microphone in **Settings → Privacy & security →
Microphone**.

**Linux** (x64 and Arm): unpack `wave9000-<version>-linux-<x64|arm64>.tar.gz`
and run `./install.sh` inside it to add wave9000 to your app menu, and
under **Open With** for sound files (`./install.sh --remove` takes it out;
nothing outside your home folder changes). It plays and records through
ALSA, so PipeWire and PulseAudio systems work through their ALSA plugin,
which desktops have. Install ffmpeg from your distribution
(`sudo apt install ffmpeg`).

## Making something

1. **Drop sound files** anywhere on the window. Each goes on a new track at
   the playhead. (Or **Import…**, then drag from Media onto a track.) Video
   files work too: their sound is used.
2. **Record**: press `r`. It records onto the armed track (the **R** on a
   track), or the selected one, or a new one, from the playhead, while the
   rest plays. Press `r` or `space` to stop. The take is a WAV file beside
   your project.
3. **Move around**: click or drag on the tracks, `←` `→` to nudge, `↑` `↓`
   for the previous or next clip edge, `space` to play. Scroll sideways;
   scroll over the track names (or hold shift) to go up and down; `cmd`+scroll
   or pinch to zoom; `cmd+0` fits everything.
4. **Arrange**: drag a clip to move it, onto another track or below the
   last to make a new one. Drag its edges to trim it. `s` splits the
   selected clip at the playhead (or every clip under it), `backspace`
   removes the selected clip. Clips snap to the playhead and other clips'
   edges; hold Alt (Option) to drag freely. Exact times and a clip's own
   gain are in the Clip panel. `cmd+z` undoes.
5. **Mix**: each track has a strip in the Mixer, with a fader, a meter,
   pan, mute (`m`), solo (`shift+s`) and arm (`shift+r`). Double-click a
   fader for 0 dB, or a pan for center; hold shift for finer moves. The
   master strip is on the right.
6. **Export**: `cmd+e`. WAV 24-bit unless `settings.toml` says otherwise;
   FLAC, MP3, AAC (m4a), Ogg and Opus through ffmpeg. Type the extension
   you want in the save dialog.

Your project is kept as you work and comes back when you reopen wave9000,
even after a crash. `cmd+s` saves it as a `.wave` file to keep.

**From splice9000**: **File → Import splice9000 Project…** (or drop a
`.splice` file on the window) puts the sound of an edit on a track of its
own, each clip where it plays in the edit. Mix it, export a WAV, and drop
that back into splice9000.

## Rearranging the window

Panels sit edge to edge, divided by a single line.

- **Drag the line** between two panels to resize them.
- **Drag a panel by its header** onto the edge of another panel to sit
  beside it, or onto its middle to swap the two.
- **Right-click a header** to add a panel next to it, replace it, or remove it.
- **View → Panels** shows or hides any panel.
- **Layout** at the top (`cmd+L`) shows every panel as a block to drag.

Panels: Tracks, Mixer, Media, Transport, Clip, Export, Log, Help, Adapters.
A panel can be shown more than once.

## Adapters

wave9000 has only what you need, and adapters add the rest: commands,
panels and export formats. Drop an adapter's zip on the window to install
it; wave9000 shows what it adds and what it may do first, and never goes
online itself. None come with wave9000 yet. To make one, see
[docs/adapters.md](docs/adapters.md).

## Customizing

There's no settings window. Everything is in plain text files in one folder
(`cmd+,` opens it):

| File | What it holds |
|---|---|
| `settings.toml` | sound output and input, recording, export format, ffmpeg's path, colors, track height, how smoothly the playhead moves |
| `keys.toml` | any key for any command (see the Help panel for the list) |
| `layout.toml` | the panels and where they are |
| `adapters/` | installed adapters |

Saved changes apply at once, with no restart. A file with a mistake falls
back to the defaults and the Log panel says what's wrong. The Log lists
your sound outputs and inputs by name when wave9000 starts. Where it is:

- macOS: `~/Library/Application Support/wave9000`
- Windows: `%APPDATA%\wave9000`
- Linux: `~/.config/wave9000`
- or wherever `WAVE9000_CONFIG_DIR` points

## Measured

On an M-series MacBook Air, release build, 2026-10-29 (0.0.2):

| | |
|---|---|
| Program size | 6.6 MB (Apple Silicon only; ffmpeg not included) |
| Memory, fresh start | 56 MB (physical footprint) |
| Memory, 4 tracks of the sample project at 96 kHz | 84 MB |
| CPU when idle | 0.0% |
| Playing those 4 tracks | about 17% of one core at the default 30 frames a second (23% at 60), most of it drawing the moving playhead and meters |
| The mixer alone, 16 tracks of cut-up clips at 48 kHz | 0.2% of one core (604× real time) |

Decoded sound is kept in memory: about 23 MB per stereo minute at 48 kHz,
46 MB at 96 kHz. That's what makes playing cost almost nothing, and it's
the first thing to change for long projects.

## Not yet

MIDI and instruments, plug-in effects, automation, a tempo grid, live
effects (adapters work offline, so they can't run in real time yet),
latency measured by itself (`record.offset_ms` is set by hand).

The Windows and Linux downloads are built, tested and started by CI, but
those machines have no sound devices, so wave9000 hasn't played sound on
Windows or Linux yet. Recording hasn't been tried with a real microphone
anywhere: its parts are tested, but the whole of it hasn't run. Run from a
terminal, macOS asks for the terminal's microphone permission.

## License

MIT
