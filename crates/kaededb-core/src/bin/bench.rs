use kaededb_core::{KaedeDb, VectorParams};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::time::Instant;
use sysinfo::{Pid, System};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000);
    let dim: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(128);
    let ef_c: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(200);
    let ef_s: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(50);
    println!(
        "benchmark: n={}, dim={}, ef_c={}, ef_s={}",
        n, dim, ef_c, ef_s
    );

    let tmp = tempfile::tempdir()?;
    let params = VectorParams {
        ef_construction: ef_c,
        ef_search: ef_s,
        max_elements: n * 2,
        ..Default::default()
    };
    let db = KaedeDb::open_with_params(tmp.path(), params)?;

    let mut rng = StdRng::seed_from_u64(42);
    let start = Instant::now();
    for _ in 0..n {
        let id = Uuid::new_v4();
        let v: Vec<f32> = (0..dim).map(|_| rng.gen()).collect();
        db.put_vector(id, v)?;
    }
    let insert_ms = start.elapsed().as_millis();
    println!("inserted {} vectors in {} ms", n, insert_ms);

    db.rebuild_vectors()?;
    let rebuild_ms = start.elapsed().as_millis() - insert_ms;
    println!("hnsw rebuild: {} ms", rebuild_ms);

    // run 10 search queries
    let q: Vec<f32> = (0..dim).map(|_| rng.gen()).collect();
    let search_start = Instant::now();
    for _ in 0..10 {
        let _ = db.search_vector(&q, 10)?;
    }
    let search_ms = search_start.elapsed().as_millis();
    println!("10 searches: {} ms (avg {} ms)", search_ms, search_ms / 10);

    // memory snapshot
    let mut sys = System::new_all();
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_process(pid);
    if let Some(p) = sys.process(pid) {
        let rss_mb = p.memory() as f64 / 1024.0 / 1024.0;
        println!("resident set: {:.2} MB", rss_mb);
    }

    Ok(())
}
