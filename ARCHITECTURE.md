# How wave9000 is built

For people changing the code. The ideals behind these choices are in
[docs/philosophy.md](docs/philosophy.md).

## The parts

| File | What it does |
|---|---|
| `main.rs` | `App`: all state, the commands, playing and recording, config, the session |
| `project.rs` | the project: tracks of clips, gains, and reading splice9000 projects |
| `engine.rs` | the mixer, and the sound output it plays through |
| `record.rs` | recording from a sound input into a WAV file |
| `media.rs` | finding ffmpeg, reading files, and waveform peaks |
| `wav.rs` | writing (and reading back) WAV files without ffmpeg |
| `export.rs` | rendering the mix to a file |
| `panels.rs` | what each panel draws |
| `layout.rs` | the panel tree, splitters, layout edit mode, `layout.toml` (from splice9000) |
| `menu.rs` | the menus: macOS's own menu bar, or drawn in the window |
| `theme.rs` | the palette every surface draws with |
| `adapters.rs` | adapters: finding, installing, approving, running, removing (from splice9000) |
| `hosting.rs` | what the app does for adapters: their requests, prompts, menus, keys |
| `keys.rs` | `COMMANDS`, the one list of commands, and `keys.toml` |
| `settings.rs` | `settings.toml` |
| `watch.rs` | notices saved config files (from Leaf) |
| `shell/` | the window, drawn by the CPU (from Folio) |
| `log.rs` | the Log panel's lines |

## One owner of state

Everything lives in `App`, on the interface thread. Four kinds of
background work exist, and none of them touches the project:

- **Reading files**: a thread per file decodes it and sends the result
  down a channel; `take_loaded` puts it where it was meant to go (only
  Media, a new track, or a given track and time, as a recorded take does).
- **Playing**: the sound device's callback plays a [`Mix`](src/engine.rs).
- **Recording**: the input's callback and a writer thread (`record.rs`).
- **Export**: a thread renders the mix into a file.

Each calls `request_repaint` when there's something new. While playing or
recording the window redraws `look.playback_fps` times a second (30) for
the playhead and meters; otherwise nothing redraws on a timer, so an idle
workstation uses no CPU.

## Sound

Every file is decoded once, by ffmpeg, to stereo 32-bit float at the
output device's own rate, and kept in memory (`Sound`). WAV files already
at that rate are read by `wav::read` instead, so recording works without
ffmpeg. Nothing is resampled or decoded while playing.

When the project changes, `App` builds a `Mix`: every track as a `Lane`
with its gain, pan, mute and solo folded into a left and a right gain, and
its clips as `Piece`s in frames, each holding an `Arc` of its file's
samples. The mix goes into a one-slot mailbox that the callback only ever
`try_lock`s: if the interface happens to hold it, the callback plays the
old mix for one more buffer. Old mixes are freed by the interface (`sent`
in `Engine`), never by the callback, which neither locks, waits, nor
allocates.

The playhead is an atomic frame count. The callback renders from it and
moves it on with a compare-and-swap, so a seek from the interface during a
buffer wins. Each lane's loudest sample goes into an atomic per track
(`Meters`), which the interface reads and lets fall at 24 dB a second.

Pan is constant-power, scaled so the center is exactly unity: an untouched
track passes through bit for bit. Where a clip starts or ends inside its
file (a cut), its sound fades over 96 frames so the cut doesn't click.

The device is opened on the first play and paused when playing stops.
With no output named in settings.toml, the stream follows the system's
output as it changes (headphones, AirPods). What a device reports goes
into `Trouble`, since its error callback can run on the sound thread: it
only counts dropouts and flags a lost device there. The interface logs
them, and on a lost device stops and opens the output again; a file still
being read at the old rate is read again at the new one.

## The maths

A track's clips are sorted by start. Clips may overlap, so their ends
aren't sorted, but the furthest any clip so far reaches is: the mixer keeps
that running maximum (`Lane::reach`) and binary-searches it for the first
clip that can sound at a time, then walks forward while clips start before
the buffer ends. The interface does the same walk without the stored
array, since it asks a few times a frame.

Waveforms come from `Peaks`: the loudest sample per 64 frames, then per 256,
1024 and so on, each level a quarter of the last. Drawing picks the
coarsest level that still has a value per point, so a whole song and a
single beat both cost a few reads per point. Values are stored as
square-root-scaled bytes, so quiet passages still show.

Faders and meters share one scale (`panels::fader_pos`): +6 dB at the top,
0 dB at 71%, -6 dB half way.

## Recording

The input's callback copies its first one or two channels into a
lock-free ring, two seconds deep, and counts anything that didn't fit. A
writer thread empties the ring into a 32-bit float WAV, and keeps the
waveform so far and a level for the interface. Stopping finishes the file
and reads it back like any other file, onto the recording track at the
time recording began (less `record.offset_ms`). An input that gives
nothing but exact zeros for a second and a half is how macOS answers when
the microphone isn't allowed, so wave9000 says where to allow it.

Takes go to `<project> takes/` beside a saved project, or to `takes/` in
the settings folder.

## Export

The mixer that plays the project renders it in 16k-frame pieces, as fast
as it goes. WAV is written by `wav::Writer` (16, 24-bit or float); every
other format is piped into ffmpeg's input as raw float samples, with the
encoder and filter the format asks for. An adapter's export format is the
same with its own arguments.

## splice9000

A `.splice` file is read by `SpliceProject`, which takes only what it
needs: each clip's media, in and out. Clips play one after another in
splice9000, so their starts are the running total of lengths; clips from
files without sound leave gaps. They go on one new track, and their files'
sound is read by ffmpeg like any other file.

## Commands

Every action is a named command in `keys::COMMANDS`. Keys, menus, buttons
and the Help panel all run them through `App::run`. Adapters' commands are
named `adapter "Name" id` and their export formats `export "Name" id`.

## Compatibility

Settings, keys, layouts and `.wave` files are read with every field
optional and unknown fields ignored, and `.wave` files carry a `wave9000`
format number. Keep it that way: old files must keep opening, and command
names, once shipped, never change meaning.
