//! What a reference comes to, without fetching any of it.
use stelly_mix::source::Sources;

fn main() -> anyhow::Result<()> {
    let reference = std::env::args().nth(1).expect("a reference");
    let sources = Sources::new(stelly_mix_qobuz::sources()?);
    let t = std::time::Instant::now();
    let pending = sources.pending(&reference)?;
    println!("{reference}: {} tracks in {:.0}s", pending.len(), t.elapsed().as_secs_f64());
    for p in pending.iter().take(12) {
        println!("  {}", p.name());
    }
    Ok(())
}
