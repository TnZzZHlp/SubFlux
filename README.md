# SubFlux / Subtitle Translator

`subtitle-translator` is a Rust terminal UI for translating video subtitles.
It first looks for a usable text subtitle track in the video; when there is no
such track, or when STT is selected explicitly, it extracts audio, requests
timestamped speech recognition, and translates the resulting segments. The
translated subtitle is written next to the video.

It uses `ffmpeg` and `ffprobe` as external programs. It does not link an FFmpeg
Rust FFI and does not use an OpenAI, Anthropic, or other provider SDK.

## Features

- Ratatui + Crossterm TUI: Home, Settings, subtitle-track selection,
  Processing, and Result/Error pages.
- An optional startup path: a video path is prefilled directly, while a
  directory is scanned recursively and opens a video-selection page when it
  contains multiple supported videos.
- Auto selection of the default text subtitle track, falling back to STT when
  no text track exists.
- Manual selection of an embedded track, an external `.srt`, `.ass`, `.ssa`,
  or `.vtt` file, or STT.
- SRT, ASS/SSA, and WebVTT parsing that replaces only translatable text.
  ASS/SSA event fields, override tags, styles, script sections, fonts,
  attachments, comments, and unknown sections remain intact.
- OpenAI-compatible chat-completions translator and STT client, plus an
  Anthropic-compatible messages translator, built directly with `reqwest` and
  `serde`.
- Strict ID-based batched translation responses, bounded retries, progress
  events, and cancellation with `C` or `Esc` on the Processing page.
- Long-video STT extracts bounded 16 kHz mono FLAC fragments with overlapping
  boundaries, then restores their source timestamps before translation.
- API-key masking in Settings and file-based `subtitle-translator.log` logging
  without authorization headers or subtitle bodies.

## Architecture

```text
terminal input -> Action -> App state -> Command/background task
                                            |
                                            v
                                      TaskEvent channel
                                            |
                                            v
                                       App state -> render

embedded/external subtitle or STT
        -> SubtitleDocument
        -> batched ID/text translation
        -> replace only subtitle text ranges
        -> <video>.<target-language>.<extension>
```

The TUI never performs probing, extraction, HTTP calls, or writing during a
render pass. Provider construction is confined to `Services::from_config`; the
pipeline only sees `Translator`, `SttProvider`, `SubtitleSource`, and
`SubtitleWriter` traits.

## Requirements and installation

- Rust stable 1.88 or later (Edition 2024).
- `ffmpeg` and `ffprobe` available on `PATH`. The application checks both on
  startup and shows a clear TUI error if either is missing.

Examples:

```bash
# Debian/Ubuntu
sudo apt install ffmpeg

# macOS with Homebrew
brew install ffmpeg

cargo build --release
cp .env.example .env
```

The binary is at `target/release/subtitle-translator`. To install it into the
Cargo bin directory instead:

```bash
cargo install --path .
```

## Configuration

Configuration is only `.env` in the current working directory. Existing system
environment variables take precedence over values in `.env`.

```dotenv
TRANSLATOR_PROVIDER=openai
TRANSLATOR_API_FORMAT=openai
TRANSLATOR_BASE_URL=https://api.example.com/v1
TRANSLATOR_API_KEY=replace-me
TRANSLATOR_MODEL=model-name
TRANSLATOR_CHUNK_SIZE=30
TRANSLATOR_CONTEXT_BEFORE=10
TRANSLATOR_CONTEXT_AFTER=5
TRANSLATOR_MAX_RETRIES=3

STT_PROVIDER=openai
STT_BASE_URL=https://api.example.com/v1
STT_API_KEY=replace-me
STT_MODEL=whisper-large-v3
STT_LANGUAGE=auto
STT_CHUNK_SECONDS=600
STT_CHUNK_OVERLAP_SECONDS=2

SOURCE_LANGUAGE=auto
TARGET_LANGUAGE=zh-CN
HTTP_TIMEOUT_SECONDS=120
OUTPUT_OVERWRITE=false
```

`TRANSLATOR_API_FORMAT=openai` calls `POST /v1/chat/completions` with a Bearer
token. `TRANSLATOR_API_FORMAT=anthropic` calls `POST /v1/messages` with
`x-api-key` and `anthropic-version`. The STT provider calls the
OpenAI-compatible `POST /v1/audio/transcriptions` endpoint as multipart form
data with `response_format=verbose_json`; a response without timestamped
`segments` is rejected.

For an existing subtitle only the translator key is needed. STT is checked when
the STT path is actually used, so its key can remain unset for subtitle-only
workflows.

STT audio is extracted as 16 kHz mono FLAC and transcribed sequentially in
bounded fragments. `STT_CHUNK_SECONDS` defaults to `600`, which keeps even
uncompressed 16 kHz mono audio safely below common 25 MB attachment limits;
`STT_CHUNK_OVERLAP_SECONDS` defaults to `2` and supplies context on both sides
of each boundary. The application assigns each recognized segment to the
middle, non-overlapping source interval and restores its absolute timestamp.
Lower `STT_CHUNK_SECONDS` if a provider imposes a smaller request-size limit.

`TRANSLATOR_CHUNK_SIZE` is the maximum number of subtitle lines that the model
must translate in one request. `TRANSLATOR_CONTEXT_BEFORE` and
`TRANSLATOR_CONTEXT_AFTER` add source-language, read-only lines before and
after that target range to preserve dialogue context. Context lines are never
included in translation progress or written back. Set both context values to
`0` to use the original adjacent-chunk behavior. The legacy
`TRANSLATOR_MAX_SEGMENTS_PER_REQUEST` variable remains accepted when the new
chunk-size variable is absent.

## Running and keyboard controls

```bash
subtitle-translator

# Start with one video already selected.
subtitle-translator /media/movie.mkv

# Recursively find videos under a directory, then choose one in the TUI.
subtitle-translator /media/series
```

On Home, type paths into the selected Video or External subtitle field. The
tool deliberately uses text paths rather than a platform-specific file picker,
so it works over SSH and in ordinary terminals.

The startup argument accepts either one video file or one directory. Directory
discovery recognizes common video extensions, sorts matches deterministically,
and never follows directory symlinks. Selecting a video always continues the
existing one-video-at-a-time workflow; it does not start a batch translation.

| Key | Action |
| --- | --- |
| `Tab`, `↑`, `↓` | Move between Home fields |
| `←`, `→`, `Enter` | Cycle source/languages or activate the selected field |
| `P` | Probe video subtitle tracks |
| `T` | Open Subtitle Track Selection |
| `A` / `X` on track page | Choose Auto / STT |
| `S` | Open Settings; `R` reloads `.env` |
| `C` or `Esc` while processing | Cancel the background task |
| `Q` | Quit |

Image tracks such as PGS, VobSub/DVD subtitles, DVB subtitles, and XSUB are
shown but never sent to the text pipeline. Select STT instead.

## Inputs and output names

Supported external subtitle formats are SRT, ASS, SSA, and WebVTT. Common
embedded text codecs such as SubRip, ASS, SSA, WebVTT, `mov_text`, and `text`
are detected. Videos are passed to FFmpeg, so the available video containers
and codecs are those supported by the local FFmpeg installation.

Existing subtitles preserve their format:

```text
/movie/Anime EP01.mkv + zh-CN + ASS
  -> /movie/Anime EP01.zh-CN.ass

/movie/movie.mkv + movie.ja.ass + zh-CN
  -> /movie/movie.zh-CN.ass
```

STT produces SRT by default. A standalone subtitle such as `movie.ja.ass`
becomes `movie.zh-CN.ass`. Existing outputs are protected unless
`OUTPUT_OVERWRITE=true` is set.

## Known limits in the first version

- No hard-subtitle OCR, PGS OCR, VobSub OCR, or other image-subtitle OCR.
- No GUI, web UI, WASM, database, directory batch translation, or video
  re-encoding. A directory passed at startup is discovery and selection only.
- No subtitle burn-in, remuxing translated subtitles into a video, or video
  re-encoding.
- STT chunks are processed sequentially and kept in memory until translation;
  disk checkpoint/resume for partial STT progress is intentionally not
  implemented.
- A translation retry keeps completed chunks in memory; disk checkpoint/resume
  is intentionally not implemented in the first version.

## Development checks

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo check --all-targets --all-features
```
