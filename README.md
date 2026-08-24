# SubFlux / Subtitle Translator

`subtitle-translator` is a Rust terminal UI for translating video subtitles.
It first looks for a usable text subtitle track in the video; when there is no
such track, or when STT is selected explicitly, it extracts audio, requests
timestamped speech recognition, and optionally translates the resulting
segments. The selected subtitle output is written next to the video.

It uses `ffmpeg` and `ffprobe` as external programs. It does not link an FFmpeg
Rust FFI and does not use an OpenAI, Anthropic, or other provider SDK.

## Features

- Ratatui + Crossterm TUI: Home, Settings, subtitle-track selection,
  Processing, and Result/Error pages.
- An optional startup path: a video path is prefilled directly, while a
  directory is scanned recursively for supported videos. The selection page
  can start a configurable-concurrency batch over every discovered video.
- Auto selection prioritizes text subtitle tracks marked SDH, then the
  default text track, falling back to STT when no text track exists.
- Manual selection of an embedded track, an external `.srt`, `.ass`, `.ssa`,
  or `.vtt` file, or STT.
- SRT, ASS/SSA, and WebVTT parsing that replaces only translatable text.
  ASS/SSA event fields, override tags, styles, script sections, fonts,
  attachments, comments, and unknown sections remain intact.
- OpenAI-compatible chat-completions translator and STT client, plus an
  Anthropic-compatible messages translator, built directly with `reqwest` and
  `serde`; translation responses use SSE streaming.
- Strict ID-based batched translation responses, bounded retries, progress
  events, and cancellation with `C` or `Esc` on the Processing page.
- Three output modes: translated text only, original-plus-translation
  bilingual subtitles, or original text only. Original-only output skips
  translator provider setup and requests.
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
        -> optional batched ID/text translation
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
SUBFLUX_TRANSLATOR_PROVIDER=openai
SUBFLUX_TRANSLATOR_API_FORMAT=openai
SUBFLUX_TRANSLATOR_BASE_URL=https://api.example.com/v1
SUBFLUX_TRANSLATOR_API_KEY=replace-me
SUBFLUX_TRANSLATOR_MODEL=model-name
SUBFLUX_TRANSLATOR_CHUNK_SIZE=30
SUBFLUX_TRANSLATOR_CONTEXT_BEFORE=10
SUBFLUX_TRANSLATOR_CONTEXT_AFTER=5
SUBFLUX_TRANSLATOR_MAX_RETRIES=3

SUBFLUX_STT_PROVIDER=openai
SUBFLUX_STT_BASE_URL=https://api.example.com/v1
SUBFLUX_STT_API_KEY=replace-me
SUBFLUX_STT_MODEL=whisper-large-v3
SUBFLUX_STT_LANGUAGE=auto
SUBFLUX_STT_CHUNK_SECONDS=600
SUBFLUX_STT_CHUNK_OVERLAP_SECONDS=2

SUBFLUX_SOURCE_LANGUAGE=auto
SUBFLUX_TARGET_LANGUAGE=zh-CN
SUBFLUX_HTTP_TIMEOUT_SECONDS=120
SUBFLUX_BATCH_CONCURRENCY=1
SUBFLUX_RUST_LOG=info
```

`SUBFLUX_TRANSLATOR_API_FORMAT=openai` calls `POST /v1/chat/completions` with a Bearer
token and strict `response_format.json_schema`. `SUBFLUX_TRANSLATOR_API_FORMAT=anthropic`
calls `POST /v1/messages` with `x-api-key`, `anthropic-version`, and Anthropic's
`output_config.format` JSON schema. If a compatible service explicitly rejects its
structured-output field, SubFlux retries that request once without the field. Authentication,
network, timeout, rate-limit, schema, and other API errors do not trigger this fallback. The
STT provider calls the OpenAI-compatible `POST /v1/audio/transcriptions` endpoint as multipart
form data with `response_format=verbose_json`; a response without timestamped `segments` is
rejected.

For an existing subtitle only the translator key is needed. STT is checked when
the STT path is actually used, so its key can remain unset for subtitle-only
workflows.

STT audio is extracted as 16 kHz mono FLAC and transcribed sequentially in
bounded fragments. `SUBFLUX_STT_CHUNK_SECONDS` defaults to `600`, which keeps even
uncompressed 16 kHz mono audio safely below common 25 MB attachment limits;
`SUBFLUX_STT_CHUNK_OVERLAP_SECONDS` defaults to `2` and supplies context on both sides
of each boundary. The application assigns each recognized segment to the
middle, non-overlapping source interval and restores its absolute timestamp.
Lower `SUBFLUX_STT_CHUNK_SECONDS` if a provider imposes a smaller request-size limit.

`SUBFLUX_TRANSLATOR_CHUNK_SIZE` is the maximum number of subtitle lines that the model
must translate in one request. `SUBFLUX_TRANSLATOR_CONTEXT_BEFORE` and
`SUBFLUX_TRANSLATOR_CONTEXT_AFTER` add source-language, read-only lines before and
after that target range to preserve dialogue context. Context lines are never
included in translation progress or written back. Set both context values to
`0` to use the original adjacent-chunk behavior. The legacy
`SUBFLUX_TRANSLATOR_MAX_SEGMENTS_PER_REQUEST` variable remains accepted when the new
chunk-size variable is absent.

## Running and keyboard controls

```bash
subtitle-translator

# Start with one video already selected.
subtitle-translator /media/movie.mkv

# Recursively find videos under a directory, then press B in the TUI to
# translate every discovered video. Set SUBFLUX_BATCH_CONCURRENCY in .env to run
# multiple videos at once.
subtitle-translator /media/series
```

Home presents an explicit **Single file / Batch** mode followed by Input,
Subtitle, Language, and Output sections. Press `Enter` on a path field to enter
editing mode, then type or paste the path. Editing supports `Left`/`Right`,
`Home`/`End`, `Backspace`, and `Delete`; press `Enter` or `Esc` to return to
navigation. The tool deliberately uses text paths rather than a
platform-specific file picker, so it works over SSH and in ordinary terminals.

The complete layout is designed for terminals of at least `80x24`. Smaller
terminals use a compact status view that keeps cancellation and overwrite
choices available.

The startup argument accepts either one video file or one directory. Directory
discovery recognizes common video extensions, sorts matches deterministically,
and never follows directory symlinks. Multiple discovered videos open Home in
Batch mode. Press `V` to inspect the video list, `Enter` there to choose one for
the single-file workflow, or `B` to process all discovered videos in order.

Batch processing supports **Auto** (each video chooses its own SDH-preferred
text track, then its default text track, and falls back to STT) and explicit
**STT**. A selected embedded-track
index or one external subtitle path cannot safely apply to unrelated videos,
so those modes are rejected for a batch. `SUBFLUX_BATCH_CONCURRENCY` controls how many
videos run at once and defaults to `1`. The Processing page shows the batch
total plus one progress bar for every active file; use `↑`/`↓` or
`PageUp`/`PageDown` to scroll when needed. Each video is independent: a
failure or an existing output is recorded and the other videos continue. `C`
or `Esc` cancels the batch and its running videos.

| Key | Action |
| --- | --- |
| `Tab`, `Shift+Tab`, `↑`, `↓` | Move between visible Home fields |
| `←`, `→`, `Enter` | Change the selected option or activate the selected field |
| `Enter` on a path | Start or finish editing; `Esc` also finishes editing |
| `←`, `→`, `Home`, `End`, `Backspace`, `Delete` while editing | Edit the path at the cursor |
| `V` | Open the discovered-video list |
| `B` | Process every video discovered from a startup directory |
| `P` | Probe video subtitle tracks without leaving the current setup page on failure |
| `T` | Open Subtitle Track Selection |
| `A` / `X` on track page | Choose Auto / STT |
| `S` | Open Settings; `R` reloads `.env` |
| `↑`, `↓`, `Home`, `End`, `PageUp`, `PageDown` | Navigate long video and track lists |
| `↑`, `↓`, `PageUp`, `PageDown` during a batch | Scroll active-file progress bars |
| `↑` / `↓` on a batch result | Select a failed video |
| `PageUp` / `PageDown` on a batch result | Scroll the selected failure details |
| `R` on a batch result | Rerun the selected failure with the original batch settings and a separate overwrite prompt |
| `C` or `Esc` while processing | Cancel the background task |
| `Q` | Quit |

Letter shortcuts are case-insensitive outside path editing mode.

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

Home offers four output types. **Bilingual (translation first)** is the default.
**Bilingual (original first)** puts the original line before its translation.
**Translated only** writes only the translation, and **Original only** writes the
extracted subtitle unchanged (or the STT transcript) without needing a
translator API key. STT still requires its own API key. All four use the same
existing `<name>.<target-language>.<extension>` output name, so select the
desired type before starting the task.

STT produces SRT by default. A standalone subtitle such as `movie.ja.ass`
becomes `movie.zh-CN.ass` in translated-only mode. Existing outputs always prompt for confirmation; `Y`/`Enter` overwrites and
`N`/`Esc` skips. In a batch, the first existing output prompts once; that
decision applies to every existing output in the batch, which is recorded as
skipped rather than failed.

## Known limits in the first version

- No hard-subtitle OCR, PGS OCR, VobSub OCR, or other image-subtitle OCR.
- No GUI, web UI, WASM, database, or video re-encoding. Directory batches
  default to one video at a time; increase `SUBFLUX_BATCH_CONCURRENCY` when the
  provider and machine can handle more.
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
