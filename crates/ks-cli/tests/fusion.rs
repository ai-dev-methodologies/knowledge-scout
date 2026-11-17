//! `ks fusion` from the command line: prefix refusal without Redis; build + batch against the test Redis (skipped without
//! KS_TEST_REDIS_URL; run through scripts/ks-redis-test.sh).
use ks_core::card::Card;
use ks_core::fusion::analyzer::{fnv1a32, tokenize};
use ks_core::fusion::features::{FEATURES, LEXICAL};
use ks_core::fusion::model::{compiled_analyzer, DIM, RECIPE};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn ks(args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ks"));
    c.args(args);
    for k in ["KS_REDIS_URL", "KS_REDIS_PASSWORD", "KS_REDIS_HOST", "KS_REDIS_PORT", "KS_KEY_PREFIX"] {
        c.env_remove(k);
    }
    c.env("KS_REDIS_URL", "redis://127.0.0.1:1");
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut p = c.spawn().unwrap();
    if let Some(s) = stdin {
        p.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    drop(p.stdin.take());
    p.wait_with_output().unwrap()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn prefixes_of_plain_ks_are_refused_before_anything_else() {
    for p in ["ks:", "ks-eval-base:", "ks-eval-new:"] {
        let o = ks(&["fusion", "build", "--data", "/nonexistent", "--model", "/nonexistent.json", "--prefix", p], &[], None);
        assert_eq!(o.status.code(), Some(2));
        assert!(err(&o).contains("refusing the key prefix"), "{p}: {}", err(&o));
        let o = ks(&["fusion", "search", "--query", "x", "--prefix", p], &[], None);
        assert!(err(&o).contains("refusing the key prefix"), "{p}: {}", err(&o));
    }
    // overlapping the configured ks prefix
    let o = ks(&["fusion", "build", "--data", "/nonexistent", "--model", "/nonexistent.json", "--prefix", "ksf-x:"], &[("KS_KEY_PREFIX", "ksf-x:")], None);
    assert!(err(&o).contains("overlaps the ks key prefix"), "{}", err(&o));
}

fn model_json() -> String {
    let lin = |names: &[&str]| json!({"kind": "linear", "names": names, "mean": vec![0.0; names.len()], "std": vec![1.0; names.len()], "w": vec![1.0; names.len()]});
    let t = "Redis 캐시 tests";
    let toks = tokenize(t, false, false);
    json!({
        "schema": "ks-fusion-model", "version": 1, "id": "test", "recipe": RECIPE, "features": FEATURES.to_vec(), "lexical_features": LEXICAL,
        "models": {"off": {"query": {"final": lin(&["q_f"]), "dense": null}, "terms": {"final": lin(&["t_f"]), "dense": null}},
                   "on": {"query": {"final": lin(&["q_f", "e_cos"]), "dense": "q"}, "terms": {"final": lin(&["e_cos"]), "dense": "qt"}}},
        "folds": {"models": {"off": {"query": [], "terms": []}, "on": {"query": [], "terms": []}}, "of": {}},
        "analyzer": compiled_analyzer(),
        "switch": {"timeout_ms": 700, "cooldown_ms": 5000, "on_timeout_ms": 10000, "min_vector_share": 0.8},
        "vectors": {"dim": DIM},
        "probes": [{"text": t, "tokens": toks, "tokens_tri_pair": tokenize(t, true, true), "hashes": toks.iter().map(|x| fnv1a32(x)).collect::<Vec<_>>()}],
    })
    .to_string()
}

fn data_dir(root: &Path) -> PathBuf {
    let data = root.join("data");
    fs::create_dir_all(data.join("cards/t")).unwrap();
    fs::create_dir_all(data.join("vocab")).unwrap();
    fs::write(data.join("vocab/terms.json"), r#"{"version":1,"terms":[{"term":"redis","facet":"entity","aliases":[]}]}"#).unwrap();
    let write = |key: &str, title: &str, skip: Option<&str>| {
        let mut c = Card::new(key);
        c.title = Some(title.into());
        c.tech_keywords = vec!["redis".into()];
        c.summary = vec![format!("{title} 요약")];
        c.skip_reason = skip.map(str::to_string);
        fs::write(data.join("cards/t").join(format!("{}.md", c.id)), c.render()).unwrap();
    };
    write("https://example.com/a", "Redis caching", None);
    write("https://example.com/b", "Redis streams", None);
    write("https://example.com/c", "Redis skipped", Some("not an article"));
    data
}

#[test]
fn build_skips_ineligible_cards_and_batch_answers_every_line() {
    let Some(url) = ks_core::testkit::test_redis_url() else { return };
    let root = std::env::temp_dir().join(format!("ks-fusion-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let data = data_dir(&root);
    fs::write(root.join("model.json"), model_json()).unwrap();
    let prefix = format!("ksf-test-cli-{}:", std::process::id());
    let env = [("KS_REDIS_URL", url.as_str())];
    let o = ks(&["fusion", "build", "--data", data.to_str().unwrap(), "--model", root.join("model.json").to_str().unwrap(), "--prefix", &prefix], &env, None);
    assert!(o.status.success(), "{}", err(&o));
    let r: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!((r["cards"].clone(), r["cards_read"].clone(), r["skipped_not_eligible"].clone()), (json!(2), json!(3), json!(1)), "{r}");
    let long = "x".repeat(600);
    let input = format!("{}\nnot json\n{}\n{}\n", json!({"qid": "ok", "query": "redis caching"}), json!({"qid": "long", "query": long}), json!({"qid": "ok2", "query": "redis", "terms": ["redis"]}));
    for sw in ["off", "on"] {
        let o = ks(&["fusion", "batch", "--prefix", &prefix, "--switch", sw, "--data", data.to_str().unwrap()], &env, Some(&input));
        assert!(o.status.success(), "{}", err(&o));
        let rows: Vec<Value> = String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(rows[1]["error"].as_str().unwrap().contains("not JSON"));
        assert!(rows[2]["error"].as_str().unwrap().contains("512"));
        if sw == "off" {
            assert_eq!(rows[0]["ids"].as_array().unwrap().len(), 2, "the skipped card is not indexed: {rows:?}");
            assert!(rows[3]["ids"].is_array());
        } else {
            // switch on with an index without vectors: an error per query, the run goes on
            assert!(rows[0]["error"].as_str().unwrap().contains("no vectors"), "{rows:?}");
            assert!(rows[3]["error"].is_string());
        }
    }
    // a rebuild while a batch runs: the next line reopens the index and is answered
    {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ks"));
        c.args(["fusion", "batch", "--prefix", &prefix, "--switch", "off"]).env_remove("KS_KEY_PREFIX").env("KS_REDIS_URL", &url);
        let mut p = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut stdin = p.stdin.take().unwrap();
        let mut out = std::io::BufReader::new(p.stdout.take().unwrap());
        let mut ask = |q: &str| -> Value {
            writeln!(stdin, "{}", json!({"qid": q, "query": "redis"})).unwrap();
            stdin.flush().unwrap();
            let mut l = String::new();
            std::io::BufRead::read_line(&mut out, &mut l).unwrap();
            serde_json::from_str(&l).unwrap()
        };
        assert!(ask("before")["ids"].is_array());
        let o = ks(&["fusion", "build", "--data", data.to_str().unwrap(), "--model", root.join("model.json").to_str().unwrap(), "--prefix", &prefix], &env, None);
        assert!(o.status.success(), "{}", err(&o));
        let after = ask("after-rebuild");
        assert!(after["ids"].is_array(), "the line after a rebuild is answered from the new index: {after}");
        assert!(ask("next")["ids"].is_array());
        // an incomplete index: per-line errors, and the run goes on
        let mut con = redis::Client::open(url.as_str()).unwrap().get_connection().unwrap();
        let _: () = redis::cmd("HSET").arg(format!("{prefix}m")).arg("complete").arg("0").query(&mut con).unwrap();
        let inc = ask("incomplete");
        assert!(inc["error"].as_str().unwrap().contains("incomplete"), "{inc}");
        assert!(ask("still-incomplete")["error"].is_string());
        let _: () = redis::cmd("HSET").arg(format!("{prefix}m")).arg("complete").arg("1").query(&mut con).unwrap();
        assert!(ask("complete-again")["ids"].is_array());
        drop(stdin);
        assert!(p.wait().unwrap().success());
    }
    // a changed card set: searching with --data refuses
    fs::write(data.join("cards/t/extra.md"), Card::new("https://example.com/d").render()).unwrap();
    let o = ks(&["fusion", "search", "--query", "redis", "--prefix", &prefix, "--data", data.to_str().unwrap()], &env, None);
    assert!(err(&o).contains("other card files"), "{}", err(&o));
    // clean up the test prefix
    let mut con = redis::Client::open(url.as_str()).unwrap().get_connection().unwrap();
    let _: redis::RedisResult<()> = redis::cmd("FT.DROPINDEX").arg(format!("{prefix}idx")).query(&mut con);
    let keys: Vec<String> = redis::cmd("KEYS").arg(format!("{prefix}*")).query(&mut con).unwrap();
    if !keys.is_empty() {
        let _: () = redis::cmd("DEL").arg(keys).query(&mut con).unwrap();
    }
    let _ = fs::remove_dir_all(&root);
}
