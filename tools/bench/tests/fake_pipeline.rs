use bench::{build_parts, render_table, run, Config};

fn fake_cfg() -> Config {
    Config {
        force_fake: true,
        stt_delay_ms: 40,
        llm_first_token_delay_ms: 60,
        llm_delta_delay_ms: 10,
        tts_delay_ms: 30,
        ..Config::default()
    }
}

#[test]
fn fake_pipeline_reports_schema_and_respects_delays() {
    let cfg = fake_cfg();
    let (parts, notes) = build_parts(&cfg).unwrap();
    for stage in ["VAD", "STT", "TTS"] {
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with(&format!("using fake {stage}"))),
            "missing fake line for {stage}: {notes:?}"
        );
    }
    let report = run(&cfg, parts).unwrap();
    let v = serde_json::to_value(&report).unwrap();

    assert_eq!(v["schema_version"], 1);
    assert!(v["host"]["os"].is_string() && v["host"]["arch"].is_string());
    assert_eq!(v["vad_detected_speech"], true);
    let names: Vec<&str> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "speech_end_to_stt_final",
            "stt_final_to_first_llm_token",
            "first_llm_token_to_first_tts_chunk"
        ]
    );
    let kinds: Vec<(&str, &str)> = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["stage"].as_str().unwrap(), c["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("vad", "fake"),
            ("stt", "fake"),
            ("llm", "fake"),
            ("tts", "fake")
        ]
    );
    let t = &v["targets"];
    assert_eq!(t["apple_silicon_ms"], 1000.0);
    assert_eq!(t["cpu_laptop_ms"], 1500.0);
    assert_eq!(t["meaningful"], false);
    assert!(t["pass"].is_boolean());

    // Stage delays are real sleeps, so each stage and the total are at least the delay.
    let stage_ms: Vec<f64> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["ms"].as_f64().unwrap())
        .collect();
    assert!(stage_ms[0] >= 40.0, "{stage_ms:?}");
    assert!(stage_ms[1] >= 60.0, "{stage_ms:?}");
    assert!(stage_ms[2] >= 20.0 + 30.0, "{stage_ms:?}"); // 2 more deltas + TTS delay
    let total = v["total_ms"].as_f64().unwrap();
    assert!(total >= cfg.delay_floor_ms() as f64, "{total}");
    assert!(total >= stage_ms.iter().sum::<f64>());

    // No audio or text content in the JSON.
    let json = serde_json::to_string(&v).unwrap();
    for leak in ["screen", "Sure", "help with that", "samples"] {
        assert!(!json.contains(leak), "JSON leaks `{leak}`");
    }
    assert!(render_table(&report).contains("not meaningful"));
}

#[test]
fn models_dir_without_features_or_models_stays_fake() {
    let cfg = Config {
        models_dir: Some("/nonexistent/models".into()),
        ..Config::default()
    };
    let (parts, notes) = build_parts(&cfg).unwrap();
    assert!(notes.iter().any(|n| n.starts_with("using fake STT")));
    assert!(run(&cfg, parts).is_ok());
}
