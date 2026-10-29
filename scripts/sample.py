"""Makes a few sample sounds with ffmpeg and a session that arranges them on
tracks, for trying wave9000 (and for screenshots).

    python3 scripts/sample.py <folder>

Then start wave9000 with WAVE9000_CONFIG_DIR=<folder>/config.
"""
import json
import os
import subprocess
import sys

out = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "sample")
media = os.path.join(out, "media")
config = os.path.join(out, "config")
os.makedirs(media, exist_ok=True)
os.makedirs(config, exist_ok=True)

sounds = [
    # name, seconds, ffmpeg source
    ("kick.wav", 8, "aevalsrc='sin(2*PI*(50+90*exp(-30*mod(t,0.5)))*t)*exp(-9*mod(t,0.5))':s=48000:d=8"),
    ("hats.wav", 8, "anoisesrc=c=white:a=0.5:d=8:r=48000,highpass=f=7000,volume='if(lt(mod(t,0.25),0.03),1,0.05)':eval=frame"),
    ("pad.wav", 12, "aevalsrc='0.18*(sin(2*PI*220*t)+sin(2*PI*277.18*t)+sin(2*PI*329.63*t))*(1-exp(-2*t))':s=48000:d=12"),
    ("bass.wav", 8, "aevalsrc='0.5*sin(2*PI*55*t)*(0.6+0.4*sin(2*PI*2*t))':s=48000:d=8"),
]
entries = []
for name, secs, source in sounds:
    path = os.path.join(media, name)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", source, "-t", str(secs), "-ac", "2", "-c:a", "pcm_s24le", path], check=True)
    entries.append({"path": path, "duration": float(secs), "rate": 48000, "channels": 2})


def clip(media, at, src_in, src_out, gain_db=0.0):
    return {"media": media, "at": at, "src_in": src_in, "src_out": src_out, "gain_db": gain_db}


session = {
    "wave9000": 1,
    "media": entries,
    "tracks": [
        {"name": "Kick", "gain_db": -2.0, "clips": [clip(0, 0.0, 0.0, 8.0), clip(0, 8.0, 0.0, 8.0)]},
        {"name": "Hats", "gain_db": -9.0, "pan": 0.35, "clips": [clip(1, 4.0, 0.0, 8.0)]},
        {"name": "Bass", "gain_db": -4.0, "clips": [clip(3, 0.0, 0.0, 4.0), clip(3, 4.0, 4.0, 8.0), clip(3, 8.0, 0.0, 8.0)]},
        {"name": "Pad", "gain_db": -8.0, "pan": -0.3, "clips": [clip(2, 2.0, 0.0, 12.0, -2.0)]},
    ],
    "master_db": 0.0,
    "playhead": 3.0,
}
with open(os.path.join(config, "session.json"), "w") as f:
    json.dump(session, f)
print(config)
