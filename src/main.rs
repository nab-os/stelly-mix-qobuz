//! stelly-mix with Qobuz: everything stelly-mix does, and `qobuz:` references
//! wherever it takes a track.

fn main() -> anyhow::Result<()> {
    stelly_mix::cli::run(vec![Box::new(stelly_mix_qobuz::Qobuz::from_env()?)])
}
