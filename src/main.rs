//! stelly-mix with Qobuz: everything stelly-mix does, and `qobuz:` and
//! `stelly:` references wherever it takes a track.

fn main() -> anyhow::Result<()> {
    stelly_mix::cli::run(stelly_mix_qobuz::sources()?)
}
