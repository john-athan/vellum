// vellum — markdown reader with real terminal typography.
//
// Interactive TUI when stdout is a tty: scroll, table of contents, search,
// link picker. Falls back to a one-shot styled dump when piped or with
// --plain (which can use the kitty text-sizing protocol for big headings).

mod dir;
mod docx;
mod imgview;
mod markdown;
mod media;
mod pdf;
mod plain;
mod sheet;
mod tui;
mod video;
mod xlsx;

use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;
use std::{env, fs};

fn main() -> ExitCode {
    let mut plain_flag = false;
    let mut path: Option<String> = None;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--plain" | "-p" => plain_flag = true,
            "-h" | "--help" => {
                eprintln!("usage: vellum [--plain] [file|dir]");
                return ExitCode::SUCCESS;
            }
            _ => path = Some(arg),
        }
    }

    // No argument browses the current directory.
    let path = path.unwrap_or_else(|| ".".to_string());

    let interactive = !plain_flag && io::stdout().is_terminal();
    let title = file_title(&path);

    // Directories open the file browser (or a plain listing when piped).
    if Path::new(&path).is_dir() {
        if interactive {
            if let Err(e) = dir::run(path) {
                eprintln!("vellum: {e}");
                return ExitCode::FAILURE;
            }
        } else {
            print!("{}", dir::dump(&path));
        }
        return ExitCode::SUCCESS;
    }

    match kind_of(&path) {
        Format::Image => {
            if interactive {
                if let Err(e) = imgview::run(title, path.clone()) {
                    eprintln!("vellum: {e}");
                    return ExitCode::FAILURE;
                }
            } else {
                match image::image_dimensions(&path) {
                    Ok((w, h)) => println!("{path}: image {w}×{h}px"),
                    Err(e) => {
                        eprintln!("vellum: {path}: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
        Format::Sheet => {
            if interactive {
                if let Err(e) = sheet::run(title, path.clone()) {
                    eprintln!("vellum: {e}");
                    return ExitCode::FAILURE;
                }
            } else {
                print!("{}", sheet::dump(&path));
            }
        }
        Format::Pdf => {
            if interactive {
                if let Err(e) = pdf::run(title, path.clone()) {
                    eprintln!("vellum: {e}");
                    return ExitCode::FAILURE;
                }
            } else {
                print!("{}", pdf::dump(&path));
            }
        }
        Format::Video => {
            if interactive {
                if let Err(e) = video::run(title, path.clone()) {
                    eprintln!("vellum: {e}");
                    return ExitCode::FAILURE;
                }
            } else {
                print!("{}", video::dump(&path));
            }
        }
        Format::Docx => {
            let src = match docx::to_markdown(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("vellum: {path}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            return render_markdown(interactive, title, src);
        }
        Format::Markdown => {
            let src = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("vellum: {path}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            return render_markdown(interactive, title, src);
        }
    }
    ExitCode::SUCCESS
}

fn render_markdown(interactive: bool, title: String, src: String) -> ExitCode {
    if interactive {
        if let Err(e) = tui::run(title, src) {
            eprintln!("vellum: {e}");
            return ExitCode::FAILURE;
        }
    } else {
        print!("{}", plain::render(&src));
    }
    ExitCode::SUCCESS
}

/// File name used as a viewer title.
fn file_title(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Open a path in its interactive viewer. Used by the directory browser, which
/// has already torn down its own terminal; each viewer sets up and restores its
/// own. Errors are printed but never abort the caller.
pub fn open_interactive(path: &str) {
    let title = file_title(path);
    let r = match kind_of(path) {
        Format::Image => imgview::run(title, path.to_string()),
        Format::Sheet => sheet::run(title, path.to_string()),
        Format::Pdf => pdf::run(title, path.to_string()),
        Format::Video => video::run(title, path.to_string()),
        Format::Docx => match docx::to_markdown(path) {
            Ok(src) => tui::run(title, src),
            Err(e) => {
                eprintln!("vellum: {path}: {e}");
                Ok(())
            }
        },
        Format::Markdown => match fs::read_to_string(path) {
            Ok(src) => tui::run(title, src),
            Err(e) => {
                eprintln!("vellum: {path}: {e}");
                Ok(())
            }
        },
    };
    if let Err(e) = r {
        eprintln!("vellum: {e}");
    }
}

pub enum Format {
    Markdown,
    Sheet,
    Image,
    Pdf,
    Video,
    Docx,
}

pub fn kind_of(path: &str) -> Format {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" => Format::Sheet,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" => Format::Image,
        "pdf" => Format::Pdf,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => Format::Video,
        "docx" => Format::Docx,
        _ => Format::Markdown,
    }
}
