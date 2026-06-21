# Contributing

Thanks for your interest in vellum.

## Development

```sh
cargo build            # debug build
cargo test             # unit tests (markdown, docx, xlsx)
cargo clippy           # lints
cargo fmt              # format
make run               # run against sample.md
```

CI runs `fmt --check`, `clippy -D warnings`, `test`, and a release build, so
please run those locally before opening a PR.

## Adding a format

Each format lives in its own module under `src/` and exposes:

- `run(title, path)` — the interactive TUI (TTY), and
- a non-interactive `dump`/`to_markdown` for piped output.

Wire it into `kind_of` / `open_interactive` in `src/main.rs`, and add a row to
the classifier in `src/dir.rs` so the directory browser colors and previews it.

## Runtime dependencies

PDF needs poppler (`pdftocairo`, `pdfinfo`, `pdftotext`); video needs `ffmpeg`
and `ffprobe`. Keep these optional — the tool should degrade gracefully when a
backend is missing.

## Scope

vellum aims to be a fast, good-looking terminal viewer for awkward-in-a-browser
files. Keep dependencies lean and the startup path quick.
