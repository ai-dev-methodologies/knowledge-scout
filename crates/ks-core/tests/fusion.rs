//! Fusion index against the test Redis (skipped without KS_TEST_REDIS_URL; run through scripts/ks-redis-test.sh): prefix
//! ownership, staleness, scope. The ranking itself is checked against the JS reference in the search lab.
use ks_core::card::Card;
use ks_core::fusion::analyzer::{fnv1a32, tokenize, Linker};
use ks_core::fusion::build::{build_index, CardInput};
use ks_core::fusion::embed::NoEmbedder;
use ks_core::fusion::features::{FEATURES, LEXICAL};
use ks_core::fusion::model::{compiled_analyzer, Model, DIM, RECIPE};
use ks_core::fusion::redis::{connect, write_index, BuildSources, Searcher};
use ks_core::fusion::switch::{Mode, Switch, SystemClock};
use ks_core::vocab::Vocab;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

static N: AtomicUsize = AtomicUsize::new(0);
static REDIS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const VOCAB: &str = r#"{"version":1,"terms":[{"term":"redis","facet":"entity","aliases":[]},{"term":"caching","facet":"domain","aliases":["cache"]}]}"#;

fn model() -> Model {
    let lin = |names: &[&str]| json!({"kind": "linear", "names": names, "mean": vec![0.0; names.len()], "std": vec![1.0; names.len()], "w": vec![1.0; names.len()]});
    let t = "Redis 캐시 tests";
    let toks = tokenize(t, false, false);
    let v = json!({
        "schema": "ks-fusion-model", "version": 1, "id": "test", "recipe": RECIPE, "features": FEATURES.to_vec(), "lexical_features": LEXICAL,
        "models": {"off": {"query": {"final": lin(&["q_f"]), "dense": null}, "terms": {"final": lin(&["t_f"]), "dense": null}},
                   "on": {"query": {"final": lin(&["q_f", "e_cos"]), "dense": "q"}, "terms": {"final": lin(&["e_cos"]), "dense": "qt"}}},
        "folds": {"models": {"off": {"query": [], "terms": []}, "on": {"query": [], "terms": []}}, "of": {}},
        "analyzer": compiled_analyzer(),
        "switch": {"timeout_ms": 700, "cooldown_ms": 5000, "on_timeout_ms": 10000, "min_vector_share": 0.8},
        "vectors": {"dim": DIM},
        "probes": [{"text": t, "tokens": toks, "tokens_tri_pair": tokenize(t, true, true), "hashes": toks.iter().map(|x| fnv1a32(x)).collect::<Vec<_>>()}],
    });
    Model::parse(&v.to_string()).unwrap()
}

fn card(key: &str, title: &str, scope: ks_core::card::Scope) -> Card {
    let mut c = Card::new(key);
    c.title = Some(title.into());
    c.tech_keywords = vec!["redis".into()];
    c.summary = vec![format!("{title} 요약")];
    c.scope = scope;
    c
}

struct Env {
    url: String,
    prefix: String,
    _serial: std::sync::MutexGuard<'static, ()>,
}
impl Env {
    fn new() -> Option<Env> {
        let url = ks_core::testkit::test_redis_url()?;
        let _serial = REDIS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Some(Env { url, prefix: format!("ksf-test-{}-{}:", std::process::id(), N.fetch_add(1, Ordering::SeqCst)), _serial })
    }
    fn con(&self) -> redis::Connection {
        connect(&self.url).unwrap()
    }
    fn build(&self, titles: &[&str]) -> ks_core::Result<ks_core::fusion::redis::BuildReport> {
        let vocab = Vocab::from_json(VOCAB, "t").unwrap();
        let scopes = [ks_core::card::Scope::Public, ks_core::card::Scope::Private];
        let cards: Vec<CardInput> = titles.iter().enumerate().map(|(i, t)| CardInput::from_card(&card(&format!("https://example.com/{i}"), t, scopes[i % 2]), &vocab, None)).collect();
        let idx = build_index(cards, &Linker::new(&vocab, None))?;
        let m = model();
        write_index(&mut self.con(), &self.prefix, &idx, &BuildSources { vocab_json: VOCAB, aliases_json: None, model: &m, vectors: None, enrich: None, extra_meta: vec![("cards_fp".into(), "fp-1".into())] })
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        let mut con = self.con();
        let _: redis::RedisResult<()> = redis::cmd("FT.DROPINDEX").arg(format!("{}idx", self.prefix)).query(&mut con);
        let keys: Vec<String> = redis::cmd("KEYS").arg(format!("{}*", self.prefix)).query(&mut con).unwrap_or_default();
        if !keys.is_empty() {
            let _: redis::RedisResult<()> = redis::cmd("DEL").arg(keys).query(&mut con);
        }
    }
}

fn off_switch(m: &Model) -> Switch {
    Switch::new(Mode::Off, &m.switch, Arc::new(NoEmbedder), Arc::new(SystemClock::new()), DIM)
}

#[test]
fn search_finds_cards_and_reports_their_scope() {
    let Some(env) = Env::new() else { return };
    let r = env.build(&["Redis caching", "Redis streams", "Kafka"]).unwrap();
    assert_eq!(r.cards, 3);
    let mut s = Searcher::open(env.con(), &env.prefix, None).unwrap();
    let sw = off_switch(&s.model);
    let res = s.search(&sw, "redis cache", None, 10, false).unwrap();
    assert!(!res.results.is_empty());
    assert_eq!(res.results[0].scope, if res.results[0].id == ks_core::card::card_id("https://example.com/0") { "public" } else { "private" });
}

#[test]
fn a_rebuild_or_an_incomplete_index_stops_the_search() {
    let Some(env) = Env::new() else { return };
    env.build(&["Redis caching", "Kafka"]).unwrap();
    let mut s = Searcher::open(env.con(), &env.prefix, None).unwrap();
    let sw = off_switch(&s.model);
    assert!(s.search(&sw, "redis", None, 10, false).is_ok());
    env.build(&["Redis caching", "Kafka", "Redis again"]).unwrap();
    let e = s.search(&sw, "redis", None, 10, false).unwrap_err().to_string();
    assert!(e.contains("rebuilt"), "{e}");
    let _: () = redis::cmd("HSET").arg(format!("{}m", env.prefix)).arg("complete").arg("0").query(&mut env.con()).unwrap();
    assert!(Searcher::open(env.con(), &env.prefix, None).is_err());
}

#[test]
fn the_cards_fingerprint_is_compared() {
    let Some(env) = Env::new() else { return };
    env.build(&["Redis caching"]).unwrap();
    let s = Searcher::open(env.con(), &env.prefix, None).unwrap();
    assert!(s.check_cards_fp("fp-1").is_ok());
    assert!(s.check_cards_fp("fp-2").unwrap_err().to_string().contains("rebuild"));
}

#[test]
fn keys_of_plain_ks_or_unknown_keys_are_never_replaced() {
    let Some(env) = Env::new() else { return };
    let mut con = env.con();
    // a ks-style key under the prefix: refused, nothing deleted
    let _: () = redis::cmd("SET").arg(format!("{}meta", env.prefix)).arg("x").query(&mut con).unwrap();
    let _: () = redis::cmd("SET").arg(format!("{}card:abc", env.prefix)).arg("y").query(&mut con).unwrap();
    let e = env.build(&["Redis caching"]).unwrap_err().to_string();
    assert!(e.contains("plain ks"), "{e}");
    let n: i64 = redis::cmd("EXISTS").arg(format!("{}meta", env.prefix)).arg(format!("{}card:abc", env.prefix)).query(&mut con).unwrap();
    assert_eq!(n, 2);
    let _: () = redis::cmd("DEL").arg(format!("{}meta", env.prefix)).arg(format!("{}card:abc", env.prefix)).query(&mut con).unwrap();
    // an unknown key without fusion meta: refused too, and named
    let _: () = redis::cmd("SET").arg(format!("{}other", env.prefix)).arg("z").query(&mut con).unwrap();
    let e = env.build(&["Redis caching"]).unwrap_err().to_string();
    assert!(e.contains(&format!("{}other", env.prefix)) && e.contains("nothing was deleted"), "{e}");
    // an index without fusion meta (no keys): refused
    let _: () = redis::cmd("DEL").arg(format!("{}other", env.prefix)).query(&mut con).unwrap();
    let _: () = redis::cmd("FT.CREATE").arg(format!("{}idx", env.prefix)).arg("ON").arg("HASH").arg("PREFIX").arg(1).arg(format!("{}nothing:", env.prefix)).arg("SCHEMA").arg("f").arg("TEXT").query(&mut con).unwrap();
    assert!(env.build(&["Redis caching"]).unwrap_err().to_string().contains("not a fusion index"));
    let _: () = redis::cmd("FT.DROPINDEX").arg(format!("{}idx", env.prefix)).query(&mut con).unwrap();
    let _: () = redis::cmd("SET").arg(format!("{}other", env.prefix)).arg("z").query(&mut con).unwrap();
    let _: () = redis::cmd("DEL").arg(format!("{}other", env.prefix)).query(&mut con).unwrap();
    // a planted fusion meta does not authorise deleting an unknown key
    let _: () = redis::cmd("HSET").arg(format!("{}m", env.prefix)).arg("schema").arg("ks-fusion-index").query(&mut con).unwrap();
    let _: () = redis::cmd("SET").arg(format!("{}session:1", env.prefix)).arg("s").query(&mut con).unwrap();
    let e = env.build(&["Redis caching"]).unwrap_err().to_string();
    assert!(e.contains("session:1"), "{e}");
    let n: i64 = redis::cmd("EXISTS").arg(format!("{}session:1", env.prefix)).query(&mut con).unwrap();
    assert_eq!(n, 1, "nothing was deleted");
    let _: () = redis::cmd("DEL").arg(format!("{}session:1", env.prefix)).arg(format!("{}m", env.prefix)).query(&mut con).unwrap();
    // plain ks's rebuild lock is a ks key
    let _: () = redis::cmd("SET").arg(format!("{}rebuild-lock", env.prefix)).arg("tok").query(&mut con).unwrap();
    assert!(env.build(&["Redis caching"]).unwrap_err().to_string().contains("rebuild-lock"));
    let _: () = redis::cmd("DEL").arg(format!("{}rebuild-lock", env.prefix)).query(&mut con).unwrap();
    // an empty prefix, then a fusion index: both may be (re)written
    env.build(&["Redis caching"]).unwrap();
    env.build(&["Redis caching", "Kafka"]).unwrap();
}
