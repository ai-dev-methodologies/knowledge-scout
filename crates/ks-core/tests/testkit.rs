//! The guard refuses a Redis without the marker before anything is written. DB 15 of the test server has no marker
//! (the marker lives in DB 0), so it stands in for an unmarked server; it must stay empty afterwards.
//! Runs wherever KS_TEST_REDIS_URL is set; on CI a missing URL fails (it is not skipped).

#[test]
fn test_redis_guard_blocks_unmarked_before_write() {
    let Some(url) = ks_core::testkit::test_redis_url() else {
        assert!(std::env::var("CI").ok().as_deref() != Some("true"), "KS_TEST_REDIS_URL missing on CI");
        return;
    };
    let mut unmarked = url::Url::parse(&url).unwrap();
    unmarked.set_path("/15");
    let e = ks_core::testkit::try_connect_test_redis(unmarked.as_str()).err().expect("unmarked DB must be refused");
    assert!(e.contains("kstest:instance"), "{e}");
    let mut con = redis::Client::open(unmarked.as_str()).unwrap().get_connection().unwrap();
    let n: u64 = redis::cmd("DBSIZE").query(&mut con).unwrap();
    assert_eq!(n, 0, "the refused connection must not have written anything");
}
