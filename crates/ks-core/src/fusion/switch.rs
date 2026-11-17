//! The embedding switch (r4-unified-switch.mjs + beginPath / settlePath of r4-unified-rank.mjs): whether one search uses
//! the embedding path ("on") or the lexical path ("off").
//!   off   never: no model call, no vector read from Redis.
//!   on    always, strictly: a failed embedding call is an error of the search (the call is given up after 10 s).
//!   auto  the embedding path while the model process answers within the timeout (700 ms); otherwise that query is ranked
//!         lexically on the candidates already fetched and the breaker OPENS: no model call and no vector read while it is
//!         open. After the cooldown (5 s) the next query starts ONE call in the background ("breaker-probe", itself still
//!         lexical); an answer in time closes the breaker, otherwise the cooldown restarts.
//! Index rules (auto): no stored vector at all ("no-vectors") or fewer than 80% of the cards with one ("few-vectors") ->
//! lexical without a model call; an embedded query whose candidates carry fewer than 80% vectors -> lexical.
//! The embedding call runs on its own thread beside the Redis round trips; `Begin::wait` collects it.
use super::embed::Embedder;
use super::model::{DenseText, Rankers, SwitchConfig};
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> f64;
}

pub struct SystemClock(Instant);
impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock(Instant::now())
    }
}
impl Default for SystemClock {
    fn default() -> Self {
        SystemClock::new()
    }
}
impl Clock for SystemClock {
    fn now_ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    On,
    Off,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        match s {
            "auto" => Ok(Mode::Auto),
            "on" => Ok(Mode::On),
            "off" => Ok(Mode::Off),
            _ => Err(Error::msg(format!("the embedding switch is one of auto, on, off (got {s:?})"))),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::On => "on",
            Mode::Off => "off",
        }
    }
}

/// The path of one query: the query vector on the embedding path, the reason in any case.
#[derive(Debug, Clone)]
pub struct Decision {
    pub q: Option<Vec<f32>>,
    pub path: &'static str,
    pub reason: &'static str,
    pub embed_ms: f64,
}

impl Decision {
    fn lexical(reason: &'static str) -> Decision {
        Decision { q: None, path: "off", reason, embed_ms: 0.0 }
    }
}

#[derive(Debug, Default)]
struct State {
    open: bool,
    opened_at: f64,
    probing: bool,
    last_error: Option<String>,
    counts: BTreeMap<&'static str, u64>,
}

impl State {
    fn count(&mut self, path: &'static str, reason: &'static str) {
        *self.counts.entry(path).or_insert(0) += 1;
        *self.counts.entry(reason).or_insert(0) += 1;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitchState {
    pub mode: &'static str,
    pub open: bool,
    pub probing: bool,
    pub last_error: Option<String>,
    pub counts: BTreeMap<&'static str, u64>,
}

pub struct Switch {
    pub mode: Mode,
    pub timeout: Duration,
    pub cooldown_ms: f64,
    pub on_timeout: Duration,
    pub min_vector_share: f64,
    /// dimension of the model's vectors; a query vector of another length is never used
    pub dim: usize,
    embed: Arc<dyn Embedder>,
    clock: Arc<dyn Clock>,
    state: Arc<Mutex<State>>,
}

type CallOut = (std::result::Result<Vec<f32>, String>, f64);

enum Pending {
    Ready(Result<Decision>),
    Call { rx: Receiver<CallOut>, limit: Duration, t0: Instant, strict: bool, dim: usize },
}

/// What `begin` decided before the candidates are fetched: whether to read the candidates' vectors, and the pending call.
pub struct Begin {
    pub want_vectors: bool,
    pending: Pending,
    state: Arc<Mutex<State>>,
    clock: Arc<dyn Clock>,
}

/// A query vector the ranking may use: the model's dimension, every value finite.
pub(crate) fn usable(v: &[f32], dim: usize) -> bool {
    v.len() == dim && v.iter().all(|x| x.is_finite())
}

/// Why a returned vector is not usable: (message, reason).
fn unusable(v: &[f32], dim: usize) -> (String, &'static str) {
    if v.len() != dim {
        (format!("the embedding vector has {} dimensions, the model expects {dim}", v.len()), "dim-mismatch")
    } else {
        ("the embedding server returned a vector with an infinite or NaN value".to_string(), "embed-error")
    }
}

fn trip(st: &mut State, now: f64, e: &str) {
    st.open = true;
    st.opened_at = now;
    st.last_error = Some(e.to_string());
}

impl Begin {
    /// Collect the decision (waits for the embedding call up to its limit).
    pub fn wait(self) -> Result<Decision> {
        match self.pending {
            Pending::Ready(r) => r,
            Pending::Call { rx, limit, t0, strict, dim } => {
                let left = limit.saturating_sub(t0.elapsed());
                // ms: how long the call took (the whole limit when it did not answer in time)
                // Err((message, reason)); ms: how long the call took (the whole limit when it did not answer in time)
                let (got, ms) = match rx.recv_timeout(left) {
                    Ok((Ok(q), ms)) if usable(&q, dim) => (Ok(q), ms),
                    Ok((Ok(q), ms)) => (Err(unusable(&q, dim)), ms),
                    Ok((Err(e), ms)) => {
                        let reason = if e.starts_with("no answer") { "embed-timeout" } else { "embed-error" };
                        (Err((e, reason)), ms)
                    }
                    Err(RecvTimeoutError::Timeout) => (Err((format!("no answer in {} ms", limit.as_millis()), "embed-timeout")), t0.elapsed().as_secs_f64() * 1000.0),
                    Err(RecvTimeoutError::Disconnected) => (Err(("the embedding call ended without an answer".to_string(), "embed-error")), t0.elapsed().as_secs_f64() * 1000.0),
                };
                let mut st = self.state.lock().unwrap();
                match got {
                    Ok(q) => {
                        st.count("on", "embedded");
                        Ok(Decision { q: Some(q), path: "on", reason: "embedded", embed_ms: ms })
                    }
                    Err((e, _)) if strict => Err(Error::msg(format!("fusion: the embedding switch is on and the embedding call failed ({e})"))),
                    Err((e, reason)) => {
                        trip(&mut st, self.clock.now_ms(), &e);
                        st.count("off", reason);
                        Ok(Decision { q: None, path: "off", reason, embed_ms: ms })
                    }
                }
            }
        }
    }
}

impl Switch {
    pub fn new(mode: Mode, cfg: &SwitchConfig, embed: Arc<dyn Embedder>, clock: Arc<dyn Clock>, dim: usize) -> Switch {
        Switch {
            mode,
            timeout: Duration::from_millis(cfg.timeout_ms),
            cooldown_ms: cfg.cooldown_ms as f64,
            on_timeout: Duration::from_millis(cfg.on_timeout_ms),
            min_vector_share: cfg.min_vector_share,
            dim,
            embed,
            clock,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    pub fn state(&self) -> SwitchState {
        let st = self.state.lock().unwrap();
        SwitchState { mode: self.mode.as_str(), open: st.open, probing: st.probing, last_error: st.last_error.clone(), counts: st.counts.clone() }
    }

    fn ready(&self, want_vectors: bool, r: Result<Decision>) -> Begin {
        Begin { want_vectors, pending: Pending::Ready(r), state: self.state.clone(), clock: self.clock.clone() }
    }

    fn call(&self, text: &str, limit: Duration) -> Receiver<CallOut> {
        let (tx, rx) = channel();
        let (embed, text) = (self.embed.clone(), text.to_string());
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let r = embed.embed(std::slice::from_ref(&text), limit).and_then(|mut v| v.pop().ok_or_else(|| "no vector".to_string()));
            let _ = tx.send((r, t0.elapsed().as_secs_f64() * 1000.0));
        });
        rx
    }

    /// Start the decision for one query text (the text of the query vector).
    pub fn begin(&self, text: &str) -> Begin {
        match self.mode {
            Mode::Off => {
                self.state.lock().unwrap().count("off", "switch-off");
                self.ready(false, Ok(Decision::lexical("switch-off")))
            }
            Mode::On => {
                let rx = self.call(text, self.on_timeout);
                Begin { want_vectors: true, pending: Pending::Call { rx, limit: self.on_timeout, t0: Instant::now(), strict: true, dim: self.dim }, state: self.state.clone(), clock: self.clock.clone() }
            }
            Mode::Auto => {
                let mut st = self.state.lock().unwrap();
                if st.open {
                    if self.clock.now_ms() - st.opened_at < self.cooldown_ms || st.probing {
                        st.count("off", "breaker-open");
                        drop(st);
                        return self.ready(false, Ok(Decision::lexical("breaker-open")));
                    }
                    st.probing = true;
                    st.count("off", "breaker-probe");
                    drop(st);
                    // one call in the background; this query is still ranked lexically
                    let rx = self.call(text, self.timeout);
                    let (state, clock, limit, dim) = (self.state.clone(), self.clock.clone(), self.timeout, self.dim);
                    std::thread::spawn(move || {
                        let r = match rx.recv_timeout(limit) {
                            Ok((Ok(q), ms)) if usable(&q, dim) && ms <= limit.as_secs_f64() * 1000.0 => Ok(()),
                            Ok((Ok(q), _)) => Err(if usable(&q, dim) { format!("no answer in {} ms", limit.as_millis()) } else { unusable(&q, dim).0 }),
                            Ok((Err(e), _)) => Err(e),
                            Err(_) => Err(format!("no answer in {} ms", limit.as_millis())),
                        };
                        let mut st = state.lock().unwrap();
                        st.probing = false;
                        match r {
                            Ok(()) => st.open = false,
                            Err(e) => trip(&mut st, clock.now_ms(), &e),
                        }
                    });
                    return self.ready(false, Ok(Decision::lexical("breaker-probe")));
                }
                drop(st);
                let rx = self.call(text, self.timeout);
                Begin { want_vectors: true, pending: Pending::Call { rx, limit: self.timeout, t0: Instant::now(), strict: false, dim: self.dim }, state: self.state.clone(), clock: self.clock.clone() }
            }
        }
    }
}

/// `termsText`: the query, plus "\n" and the client terms joined by ", " when there are any.
pub fn terms_text(query: &str, terms: Option<&[String]>) -> String {
    match terms {
        Some(ts) if !ts.is_empty() => format!("{query}\n{}", ts.join(", ")),
        _ => query.to_string(),
    }
}

/// The decision of one query before the candidates are fetched. `mode`: 0 query, 1 terms. `vector_share`: the share of
/// the index's cards that carry a vector.
pub fn begin_path(sw: &Switch, rankers: &Rankers, mode: usize, vector_share: f64, query: &str, terms: Option<&[String]>) -> Result<Begin> {
    if sw.mode == Mode::Off {
        return Ok(sw.begin(query));
    }
    if !(vector_share > 0.0) {
        if sw.mode == Mode::On {
            return Err(Error::msg("fusion: the embedding switch is on but the index holds no vectors"));
        }
        return Ok(sw.ready(false, Ok(Decision::lexical("no-vectors"))));
    }
    if sw.mode == Mode::Auto && vector_share < sw.min_vector_share {
        return Ok(sw.ready(false, Ok(Decision::lexical("few-vectors"))));
    }
    let text = match rankers.on[mode].dense {
        Some(DenseText::Qt) => terms_text(query, terms),
        _ => query.to_string(),
    };
    Ok(sw.begin(&text))
}

/// The decision once the candidates are there: an embedded query whose candidates carry too few vectors is ranked
/// lexically (auto), or is an error when none carries one (on).
pub fn settle_path(sw: &Switch, r: Decision, vecs: &[Option<Vec<f32>>]) -> Result<Decision> {
    if r.q.is_none() {
        return Ok(r);
    }
    let have = vecs.iter().filter(|v| v.is_some()).count();
    if sw.mode == Mode::On {
        if have == 0 {
            return Err(Error::msg("fusion: the embedding switch is on but no candidate of this query has a stored vector"));
        }
        return Ok(r);
    }
    if have > 0 && have as f64 >= sw.min_vector_share * vecs.len() as f64 {
        Ok(r)
    } else {
        Ok(Decision { q: None, path: "off", reason: if have > 0 { "few-vectors" } else { "no-vectors" }, embed_ms: r.embed_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    struct FakeClock(AtomicU64);
    impl Clock for FakeClock {
        fn now_ms(&self) -> f64 {
            self.0.load(Ordering::SeqCst) as f64
        }
    }
    impl FakeClock {
        fn set(&self, ms: u64) {
            self.0.store(ms, Ordering::SeqCst)
        }
    }

    /// Answers [1, 0] after `delay_ms`, or fails when `fail` is set; counts calls.
    struct Fake {
        calls: AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
        delay_ms: AtomicU64,
        vec: Mutex<Vec<f32>>,
    }
    impl Fake {
        fn new() -> Arc<Fake> {
            Arc::new(Fake { calls: AtomicUsize::new(0), fail: false.into(), delay_ms: AtomicU64::new(0), vec: Mutex::new(vec![1.0, 0.0]) })
        }
    }
    impl Embedder for Fake {
        fn embed(&self, texts: &[String], _: Duration) -> std::result::Result<Vec<Vec<f32>>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(self.delay_ms.load(Ordering::SeqCst)));
            if self.fail.load(Ordering::SeqCst) {
                return Err("connection refused".into());
            }
            Ok(texts.iter().map(|_| self.vec.lock().unwrap().clone()).collect())
        }
    }

    fn cfg() -> SwitchConfig {
        SwitchConfig { timeout_ms: 80, cooldown_ms: 5000, on_timeout_ms: 500, min_vector_share: 0.8 }
    }
    fn sw(mode: Mode, e: Arc<Fake>, c: Arc<FakeClock>) -> Switch {
        Switch::new(mode, &cfg(), e, c, 2)
    }
    fn wait_probe(s: &Switch) {
        for _ in 0..200 {
            if !s.state().probing {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the probe never finished");
    }

    #[test]
    fn off_never_calls_the_model() {
        let (e, c) = (Fake::new(), Arc::new(FakeClock(AtomicU64::new(0))));
        let s = sw(Mode::Off, e.clone(), c);
        let b = s.begin("q");
        assert!(!b.want_vectors);
        let d = b.wait().unwrap();
        assert_eq!((d.path, d.reason, d.q.is_none()), ("off", "switch-off", true));
        assert_eq!(e.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn on_is_strict() {
        let (e, c) = (Fake::new(), Arc::new(FakeClock(AtomicU64::new(0))));
        let s = sw(Mode::On, e.clone(), c);
        let d = s.begin("q").wait().unwrap();
        assert_eq!((d.path, d.reason), ("on", "embedded"));
        e.fail.store(true, Ordering::SeqCst);
        let err = s.begin("q").wait().unwrap_err().to_string();
        assert!(err.contains("switch is on") && err.contains("connection refused"), "{err}");
        // a slow answer is still used: the auto timeout does not apply
        e.fail.store(false, Ordering::SeqCst);
        e.delay_ms.store(150, Ordering::SeqCst);
        assert_eq!(s.begin("q").wait().unwrap().path, "on");
    }

    #[test]
    fn auto_breaker_opens_waits_the_cooldown_and_closes_on_a_good_probe() {
        let (e, c) = (Fake::new(), Arc::new(FakeClock(AtomicU64::new(1000))));
        let s = sw(Mode::Auto, e.clone(), c.clone());
        let b = s.begin("q");
        assert!(b.want_vectors);
        assert_eq!(b.wait().unwrap().reason, "embedded");
        e.fail.store(true, Ordering::SeqCst);
        let d = s.begin("q").wait().unwrap();
        assert_eq!((d.path, d.reason), ("off", "embed-error"));
        assert!(s.state().open);
        // open: no model call, no vector read
        let calls = e.calls.load(Ordering::SeqCst);
        c.set(1000 + 4999);
        let b = s.begin("q");
        assert!(!b.want_vectors);
        assert_eq!(b.wait().unwrap().reason, "breaker-open");
        assert_eq!(e.calls.load(Ordering::SeqCst), calls);
        // after the cooldown: one background probe; it fails, so the cooldown restarts from now
        c.set(1000 + 5001);
        assert_eq!(s.begin("q").wait().unwrap().reason, "breaker-probe");
        wait_probe(&s);
        assert_eq!(e.calls.load(Ordering::SeqCst), calls + 1);
        assert!(s.state().open);
        c.set(1000 + 5001 + 4000);
        assert_eq!(s.begin("q").wait().unwrap().reason, "breaker-open");
        // the server is back: the next probe closes the breaker, the query after it is embedded
        e.fail.store(false, Ordering::SeqCst);
        c.set(1000 + 5001 + 5002);
        assert_eq!(s.begin("q").wait().unwrap().reason, "breaker-probe");
        wait_probe(&s);
        assert!(!s.state().open);
        assert_eq!(s.begin("q").wait().unwrap().reason, "embedded");
        let counts = s.state().counts;
        assert_eq!((counts["on"], counts["off"], counts["breaker-probe"], counts["breaker-open"]), (2, 5, 2, 2));
    }

    #[test]
    fn auto_gives_up_at_the_timeout() {
        let (e, c) = (Fake::new(), Arc::new(FakeClock(AtomicU64::new(0))));
        e.delay_ms.store(400, Ordering::SeqCst);
        let s = sw(Mode::Auto, e, c);
        let t0 = Instant::now();
        let d = s.begin("q").wait().unwrap();
        assert_eq!((d.path, d.reason), ("off", "embed-timeout"));
        assert!(t0.elapsed() < Duration::from_millis(300), "{:?}", t0.elapsed());
        assert!(s.state().last_error.unwrap().contains("no answer in 80 ms"));
    }

    #[test]
    fn index_and_candidate_vector_shares() {
        let (e, c) = (Fake::new(), Arc::new(FakeClock(AtomicU64::new(0))));
        let auto = sw(Mode::Auto, e.clone(), c.clone());
        let on = sw(Mode::On, e.clone(), c);
        let q = Decision { q: Some(vec![1.0]), path: "on", reason: "embedded", embed_ms: 1.0 };
        let v = |have: usize, n: usize| -> Vec<Option<Vec<f32>>> { (0..n).map(|i| if i < have { Some(vec![1.0]) } else { None }).collect() };
        assert_eq!(settle_path(&auto, q.clone(), &v(8, 10)).unwrap().path, "on");
        assert_eq!(settle_path(&auto, q.clone(), &v(7, 10)).unwrap().reason, "few-vectors");
        assert_eq!(settle_path(&auto, q.clone(), &v(0, 10)).unwrap().reason, "no-vectors");
        assert_eq!(settle_path(&on, q.clone(), &v(1, 10)).unwrap().path, "on");
        assert!(settle_path(&on, q, &v(0, 10)).is_err());
        assert_eq!(terms_text("q", Some(&["a".into(), "b".into()])), "q\na, b");
        assert_eq!(terms_text("q", Some(&[])), "q");
    }
    #[test]
    fn wrong_dimension_or_non_finite_vectors_are_not_used() {
        let (e, c) = (Fake::new(), Arc::new(FakeClock(AtomicU64::new(0))));
        *e.vec.lock().unwrap() = vec![1.0, 0.0, 0.0];
        let s = sw(Mode::Auto, e.clone(), c.clone());
        let d = s.begin("q").wait().unwrap();
        assert_eq!((d.path, d.reason), ("off", "dim-mismatch"));
        assert!(s.state().open);
        let err = sw(Mode::On, e.clone(), c.clone()).begin("q").wait().unwrap_err().to_string();
        assert!(err.contains("dimension"), "{err}");
        for bad in [vec![f32::NAN, 0.0], vec![0.5, f32::INFINITY]] {
            *e.vec.lock().unwrap() = bad;
            let d = sw(Mode::Auto, e.clone(), c.clone()).begin("q").wait().unwrap();
            assert_eq!((d.path, d.reason), ("off", "embed-error"));
        }
        // a vector with a NaN in the middle is not usable either
        *e.vec.lock().unwrap() = vec![0.5, 0.5];
        assert_eq!(sw(Mode::Auto, e.clone(), c).begin("q").wait().unwrap().reason, "embedded");
        assert!(!usable(&[0.5, f32::NAN, 0.5], 3) && usable(&[0.5, 0.1, 0.5], 3));
    }
}
