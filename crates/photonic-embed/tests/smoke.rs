#[test]
#[ignore]
fn embeds_and_ranks() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let e = runtime
        .block_on(async { photonic_embed::Embedder::new() })
        .expect("load model from a Tokio caller");
    let corpus = vec![
        "Draw ellipses and circles".to_string(),
        "Add text to the canvas".to_string(),
        "Undo the last action".to_string(),
    ];
    let vecs = e.embed(&corpus).expect("embed corpus");
    assert_eq!(vecs.len(), 3);
    for vector in &vecs {
        assert_eq!(vector.len(), 384, "MiniLM is 384-dim");
        assert!(vector.iter().all(|value| value.is_finite()));
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "normalized embedding: {norm}");
    }
    let q = e.embed(&["round shape".to_string()]).expect("embed query");
    let sims: Vec<f32> = vecs
        .iter()
        .map(|v| photonic_embed::cosine(&q[0], v))
        .collect();
    println!("sims (ellipse/text/undo) = {sims:?}");
    assert!(
        sims[0] > sims[1] && sims[0] > sims[2],
        "ellipse should win: {sims:?}"
    );
    if std::env::var_os("PHOTONIC_EMBED_CACHE_ONLY").is_none() {
        // A fresh process must load all cached weights/tokenizer files without
        // reaching the downloader's endpoint after the first successful load.
        let cached = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "embeds_and_ranks", "--ignored", "--nocapture"])
            .env("PHOTONIC_EMBED_CACHE_ONLY", "1")
            .env("HF_ENDPOINT", "https://127.0.0.1:9")
            .status()
            .expect("cached model process");
        assert!(cached.success(), "offline cached model loading failed");
    }
}
