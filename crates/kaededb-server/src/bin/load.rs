use rand::{rngs::StdRng, Rng, SeedableRng};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let url = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:8000".into());
    let dim: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(768);
    let n_vectors: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5000);
    let searches: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(200);
    println!(
        "load: url={}, dim={}, n_vectors={}, searches={}",
        url, dim, n_vectors, searches
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let mut rng = StdRng::seed_from_u64(1234);

    // insert vectors
    for _ in 0..n_vectors {
        let id = uuid::Uuid::new_v4();
        let v: Vec<f32> = (0..dim).map(|_| rng.gen()).collect();
        let body = serde_json::json!({ "id": id, "vector": v });
        client
            .post(format!("{}/v1/vector", url))
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
    }

    // warm hnsw
    let _ = client
        .post(format!("{}/v1/vector/rebuild", url))
        .send()
        .await?;

    // run searches
    let mut times = Vec::with_capacity(searches);
    for _ in 0..searches {
        let q: Vec<f32> = (0..dim).map(|_| rng.gen()).collect();
        let body = serde_json::json!({ "query": q, "k": 10, "ef_search": 80 });
        let start = Instant::now();
        let resp = client
            .post(format!("{}/v1/vector/search", url))
            .json(&body)
            .send()
            .await?;
        resp.error_for_status()?;
        times.push(start.elapsed());
    }

    times.sort();
    let p =
        |pct: f32| times[(pct * times.len() as f32).clamp(0.0, (times.len() - 1) as f32) as usize];
    let mean = times.iter().map(|d| d.as_secs_f64()).sum::<f64>() / times.len() as f64;
    println!(
        "p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, mean {:.2} ms",
        p(0.50).as_secs_f64() * 1000.0,
        p(0.95).as_secs_f64() * 1000.0,
        p(0.99).as_secs_f64() * 1000.0,
        mean * 1000.0
    );
    Ok(())
}
