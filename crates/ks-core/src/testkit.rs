//! Guard for tests that write to Redis. Tests may only run against the dedicated test instance: it is never the
//! personal server (port 6390) nor the endpoint `KS_REDIS_URL` points at, and it carries the marker key
//! `kstest:instance` = `1`, which is read before anything is written. This is a defensive layer: the supported way to
//! run these tests is the wrapper `scripts/ks-redis-test.sh`, which also serializes them across processes.

use url::Url;

/// The personal Redis host port (docker-compose.yml default in this setup). A test URL on it is always refused.
pub const PERSONAL_PORT: u16 = 6390;
/// Marker key (DB 0 of the test server) and the only accepted value.
pub const MARKER_KEY: &str = "kstest:instance";

fn endpoint(url: &str) -> Result<(String, u16), String> {
    let u = Url::parse(url).map_err(|_| "not a valid redis URL".to_string())?;
    let host = u.host_str().ok_or("redis URL has no host")?.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    // Loopback aliases (and the unspecified address, which connects to loopback) are one endpoint.
    let host = match host.as_str() {
        "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" | "::" => "loopback".to_string(),
        h if h.starts_with("127.") => "loopback".to_string(),
        _ => host,
    };
    Ok((host, u.port().unwrap_or(6379)))
}

/// Pure check; `personal_url` is the personal endpoint (`KS_REDIS_URL`), if any. Messages never include the URL
/// (it embeds the password).
pub fn check_test_redis_url(test_url: &str, personal_url: Option<&str>) -> Result<(), String> {
    let (host, port) = endpoint(test_url).map_err(|e| format!("KS_TEST_REDIS_URL: {e}"))?;
    if port == PERSONAL_PORT {
        return Err(format!("KS_TEST_REDIS_URL uses port {PERSONAL_PORT}, the personal Redis; tests must use the test instance"));
    }
    if let Some(p) = personal_url.filter(|p| !p.is_empty()) {
        if endpoint(p).is_ok_and(|e| e == (host, port)) {
            return Err("KS_TEST_REDIS_URL points at the same endpoint as KS_REDIS_URL (the personal Redis)".into());
        }
    }
    Ok(())
}

/// Pure check of the value read from `MARKER_KEY`: only `Some("1")` passes.
pub fn check_marker_value(v: Option<&str>) -> Result<(), String> {
    match v {
        Some("1") => Ok(()),
        _ => Err(format!("{MARKER_KEY} is not 1 on the test Redis: refusing to write (create the marker only on the test instance)")),
    }
}

/// Connect and verify the marker with a read; nothing is written before it passes.
pub fn try_connect_test_redis(url: &str) -> Result<redis::Connection, String> {
    let mut con = redis::Client::open(url).and_then(|c| c.get_connection()).map_err(|e| format!("test redis: connect failed: {}", e.category()))?;
    let v: Option<String> = redis::cmd("GET").arg(MARKER_KEY).query(&mut con).map_err(|e| format!("test redis: marker read failed: {}", e.category()))?;
    check_marker_value(v.as_deref())?;
    Ok(con)
}

/// Panics instead of returning: a test must never continue against an unverified server.
pub fn connect_test_redis(url: &str) -> redis::Connection {
    try_connect_test_redis(url).unwrap_or_else(|e| panic!("{e}"))
}

/// The test Redis URL, or None when the variable is unset or empty. A set URL is checked (panic on refusal).
/// The marker itself is verified by `connect_test_redis` / `try_connect_test_redis`.
pub fn test_redis_url() -> Option<String> {
    let url = std::env::var("KS_TEST_REDIS_URL").ok().filter(|u| !u.is_empty())?;
    if let Err(e) = check_test_redis_url(&url, std::env::var("KS_REDIS_URL").ok().as_deref()) {
        panic!("{e}");
    }
    // Every test that gets a URL also passes the marker check before its first write.
    drop(connect_test_redis(&url));
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redis_url_rejects_same_endpoint_localhost_alias() {
        let personal = Some("redis://:pw@127.0.0.1:7777");
        for alias in ["redis://:x@localhost:7777", "redis://:x@127.0.0.1:7777/3", "redis://:x@[::1]:7777", "redis://:x@127.1.2.3:7777"] {
            let e = check_test_redis_url(alias, personal).unwrap_err();
            assert!(e.contains("same endpoint") && !e.contains("pw"), "{alias}: {e}");
        }
        assert!(check_test_redis_url("redis://:x@127.0.0.1:6391", personal).is_ok());
        // default port is 6379 when omitted
        assert!(check_test_redis_url("redis://localhost", Some("redis://127.0.0.1:6379")).is_err());
    }

    #[test]
    fn test_redis_url_rejects_personal_port() {
        for u in ["redis://:x@127.0.0.1:6390", "redis://:x@localhost:6390/0", "redis://:x@db.internal:6390"] {
            assert!(check_test_redis_url(u, None).unwrap_err().contains("6390"), "{u}");
        }
        assert!(check_test_redis_url("not a url", None).is_err());
    }

    #[test]
    fn marker_value_rejects_missing_or_wrong() {
        for v in [None, Some(""), Some("0"), Some("true"), Some("1 "), Some("2")] {
            assert!(check_marker_value(v).is_err(), "{v:?}");
        }
    }

    #[test]
    fn marker_value_accepts_one() {
        assert!(check_marker_value(Some("1")).is_ok());
    }
}
