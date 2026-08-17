# AGENTS.md

## Project overview

`subflux` (binary `subtitle-translator`) is a Rust terminal UI that translates
video subtitles: it extracts a text subtitle track (or runs speech-to-text on
the audio when no text track exists or STT is chosen), translates the text with
an LLM, and writes a translated subtitle file next to the video. It talks to
OpenAI-compatible and Anthropic-compatible HTTP APIs directly with `reqwest`
(no provider SDK), and shells out to `ffmpeg`/`ffprobe` for probing, extraction,
and audio (no FFmpeg FFI).

The current source already implements the "context-aware translation" pipeline
described in `PLAN.md` (chunk `previous_context` / `segments` / `next_context`,
source-only context, strict ID-based response validation). `PLAN.md` is a
specification of that already-landed design; treat it as design intent, not an
outstanding todo. The README's "Development checks" list is the canonical gate.

## Layout

- `src/main.rs` — binary entry: TUI event loop, command execution, file logging.
- `src/lib.rs` — library crate (`subflux`) exposing all app modules; the binary is a thin client.
- `src/action.rs`, `src/app.rs` — user input becomes `Action`, then `Command`; TUI renders `App` state.
- `src/event.rs` — `TaskEvent` channel from background commands to the TUI.
- `src/pipeline/` — `run_pipeline`, `translate_document` (chunk loop, retries, progress/events), STT document assembly.
- `src/translator/` — `chunk.rs` (context chunks), `prompt.rs` (system prompt + payload), `model.rs` (`TranslationRequest`/`Response` + strict validator), `provider.rs` (`Translator` trait), `openai.rs`, `anthropic.rs` (wire clients).
- `src/subtitle/` — `model.rs` (`SubtitleDocument`), `srt.rs`/`ass.rs`/`vtt.rs` + `token.rs` (formatting-tag template round-trip), `source.rs` (embedded/external), `writer.rs` (atomic write, no-clobber), `lines.rs`.
- `src/stt/` — `provider.rs` trait, `openai.rs` client (verbose_json timestamped segments).
- `src/media/` — `ffmpeg.rs`, `ffprobe.rs`, `audio.rs`, `model.rs` (track kinds).
- `src/output/path.rs` — output-name derivation (`<video>.<lang>.<ext>`).
- `src/tui/` — `terminal.rs`, `input.rs`, `ui.rs`, `pages/` (home, settings, tracks, processing, result).
- `src/config.rs` — `.env` loading and validation; `LanguageCode`, `ApiKey` (masked/safe).
- `src/error.rs` — `AppError` with `safe_message()` for TUI/log display.
- `src/services.rs` — the only provider-construction boundary (`Services::from_config`).

## Commands

- Install / build: `cargo build` (add `--release` for `target/release/subtitle-translator`; `cargo install --path .` also works).
- Runtime: needs `ffmpeg` and `ffprobe` on `PATH`, and a `.env` in the current working directory (`cp .env.example .env`, then edit keys/URLs).
- Test: `cargo test --all-features` (verification run: `cargo test --all-features --locked`).
- Format: `cargo fmt --check`.
- Check: `cargo check --all-targets --all-features`.
- Lint: `cargo lint` — required by the project. IMPORTANT: it is a local cargo
  alias (`clippy --allow-dirty --fix --all-targets --all-features -- -D clippy::pedantic -D clippy::nursery -D clippy::cargo ...`). It actively applies
  clippy auto-fixes to source files AND its fixed output can break `cargo fmt
  --check` (e.g. it rewrites `.map_or_else`/`match` forms into unformatted
  one-liners). It can exit 0 while leaving clippy warnings. For a non-mutating
  gate that passes on the committed code: `cargo clippy --no-deps --all-targets --all-features -- -D warnings` (verified: clean exit 0). If you run `cargo lint`,
  re-run `cargo fmt` afterward to restore formatting.

## Conventions

- Rust 2024 edition, MSRV 1.88. Async via `tokio` + `async_trait`; errors via `thiserror` (`AppError`), always surfaced to the TUI through `safe_message()`.
- Layering: the TUI never probes, extracts, writes, or does HTTP during a render pass. Providers are constructed only in `Services::from_config`; the pipeline talks to `Translator` / `SttProvider` / `SubtitleSource` / `SubtitleWriter` traits — no `if openai { ... } else if anthropic` branching.
- Subtitle round-trip: parsers keep the full original document and record byte ranges for translatable text; rendering replaces only those ranges (ASS/SSA styles, override tags, headers, attachments, and unknown sections stay intact). Never send timestamps, styles, or formatting tags to an LLM.
- Translation pipeline invariants: context (`previous_context` / `next_context`) is read-only, always source-language, never translated or written back; each subtitle ID is translated and written back exactly once; responses are strictly validated (exact ID set, count, order, no duplicates, no timestamps, no context IDs) before write-back; progress counts only `segments`, not context.
- Secrets: API keys are masked in the TUI and redacted from errors/logs; logs must never include authorization headers or subtitle bodies.
- Tests: unit tests live in each module's `#[cfg(test)] mod tests`; no external test framework; `#[tokio::test]` for async; `tempfile` for filesystem tests. Keep tests alongside the code they cover.

## Pitfalls

- Never run `cargo lint` and leave its output unformatted; re-run `cargo fmt` after. Prefer the non-mutating `cargo clippy ... -D warnings` form for verification.
- The binary is `subtitle-translator` (Cargo.toml `[[bin]]`), while the library crate is `subflux`; code imports `subflux::…` from the binary.
- PATH-dependent tests: `media::ffmpeg::tests::probes_and_extracts_a_real_embedded_srt_when_ffmpeg_is_available` requires `ffmpeg`/`ffprobe` on PATH; the rest do not.
- `.gitignore` excludes `target/`, the local `.env`, and `subtitle-translator.log` — never commit those.
- `PLAN.md` describes the already-implemented context feature; do not re-implement or treat it as pending work.