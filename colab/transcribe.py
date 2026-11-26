import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path("/content")
AUDIO = ROOT / "subflux-audio.flac"
SCRIPT = ROOT / "subflux-transcribe.py"


def run(language, result):
    runtime = ROOT / "subflux-stt-env"
    python = runtime / "bin/python"
    ready = runtime / "ready"
    if not ready.exists():
        print("Installing Whisper GPU dependencies", flush=True)
        subprocess.run([sys.executable, "-m", "venv", str(runtime)], check=True)
        subprocess.run(
            [
                str(python), "-m", "pip", "install", "--quiet",
                "faster-whisper==1.2.1", "ctranslate2==4.6.3",
                "nvidia-cublas-cu12==12.8.4.1", "nvidia-cudnn-cu12==9.10.2.21",
            ],
            check=True,
        )
        ready.touch()

    site = Path(subprocess.check_output(
        [str(python), "-c", "import sysconfig; print(sysconfig.get_path('purelib'))"],
        text=True,
    ).strip())
    env = os.environ.copy()
    env["LD_LIBRARY_PATH"] = os.pathsep.join(filter(None, [
        str(site / "nvidia/cublas/lib"),
        str(site / "nvidia/cudnn/lib"),
        env.get("LD_LIBRARY_PATH"),
    ]))
    subprocess.run([str(python), str(SCRIPT), language, result], env=env, check=True)


def transcribe(language, result):
    from faster_whisper import WhisperModel

    print("Transcribing with Whisper large-v3 (downloads weights on first use)", flush=True)
    model = WhisperModel("large-v3", device="cuda", compute_type="int8_float16")
    segments, info = model.transcribe(
        str(AUDIO),
        task="transcribe",
        language=language or None,
        vad_filter=True,
    )
    cues = []
    for segment in segments:
        text = " ".join(segment.text.split())
        if text and segment.end > segment.start:
            cues.append({"start": segment.start, "end": segment.end, "text": text})
    Path(result).write_text(
        json.dumps({"language": info.language if cues else None, "segments": cues}),
        encoding="utf-8",
    )


if __name__ == "__main__":
    transcribe(sys.argv[1], sys.argv[2])
