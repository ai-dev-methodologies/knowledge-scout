//! CI gate guard. The Redis and real-browser tests return silently when their opt-in variables are
//! missing, so a CI run that lost them would be green without having run them. On CI (`CI=true`)
//! both must be present. Only names are reported, never values (the Redis URL embeds a password).

#[test]
fn ci_runs_redis_and_browser_tests() {
    if std::env::var("CI").ok().as_deref() != Some("true") {
        return;
    }
    let mut missing = Vec::new();
    if std::env::var("KS_TEST_REDIS_URL").ok().filter(|u| !u.is_empty()).is_none() {
        missing.push("KS_TEST_REDIS_URL (non-empty)");
    }
    if std::env::var("KS_TEST_BROWSER").ok().as_deref() != Some("1") {
        missing.push("KS_TEST_BROWSER=1");
    }
    assert!(
        missing.is_empty(),
        "CI would pass without running the Redis/browser tests; missing: {}",
        missing.join(", ")
    );
}
