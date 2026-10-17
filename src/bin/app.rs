//! stelly-mix's window with Qobuz: `qobuz:` and `stelly:` references in its
//! Index box, wherever the command line takes them.

// Without this a release build on Windows opens a console next to the window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    stelly_mix_app::run(stelly_mix_qobuz::sources()?);
    Ok(())
}
