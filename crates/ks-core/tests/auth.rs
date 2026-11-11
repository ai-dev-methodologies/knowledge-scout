//! API keys for the remote MCP server. Pure tests always run; Redis tests run only when KS_TEST_REDIS_URL is set and
//! use a unique key prefix that is purged afterwards.
use ks_core::auth::{key_hash, new_key, AuthStore, KeyStore, RedisAuthStore, Role};
use ks_core::card::Card;
use ks_core::index::{Fingerprints, Index};
use ks_core::vocab::Vocab;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

struct Purge(String, String);
impl Drop for Purge {
    fn drop(&mut self) {
        if let Ok(mut i) = Index::connect(&self.0, &self.1) {
            let _ = i.purge();
        }
    }
}

fn redis() -> Option<(String, String, Purge)> {
    let url = ks_core::testkit::test_redis_url()?;
    let prefix = format!("kstest-auth-{}-{}:", std::process::id(), N.fetch_add(1, Ordering::SeqCst));
    let purge = Purge(url.clone(), prefix.clone());
    Some((url, prefix, purge))
}

#[test]
fn new_keys_are_random_and_hash_is_sha256_hex() {
    let (a, b) = (new_key().unwrap(), new_key().unwrap());
    assert_ne!(a, b);
    for k in [&a, &b] {
        assert!(k.starts_with("ks_") && k.len() == 3 + 64 && k[3..].bytes().all(|c| c.is_ascii_hexdigit()), "{}", k.len());
    }
    let h = key_hash(&a);
    assert_eq!(h.len(), 64);
    assert!(h.bytes().all(|c| c.is_ascii_hexdigit()) && !h.contains(&a[3..]));
    assert_eq!(h, key_hash(&a), "deterministic");
    assert_ne!(h, key_hash(&b));
}

#[test]
fn roles_are_ordered_and_parse() {
    assert!(Role::Reader < Role::Admin && Role::Admin < Role::Owner);
    for r in [Role::Reader, Role::Admin, Role::Owner] {
        assert_eq!(Role::parse(r.as_str()), Some(r));
    }
    assert_eq!(Role::parse("root"), None);
}

#[test]
fn issue_lookup_list_revoke() {
    let Some((url, prefix, _purge)) = redis() else { return };
    let mut ks = KeyStore::connect(&url, &prefix).unwrap();
    assert_eq!(ks.count_active(Role::Reader).unwrap(), 0);
    let (meta, raw) = ks.issue(Role::Admin, "demo", "alice", "2026-11-12T00:00:00Z").unwrap();
    assert_eq!((meta.role, meta.purpose.as_str(), meta.user.as_str(), meta.revoked_at.as_deref()), (Role::Admin, "demo", "alice", None));
    let got = ks.lookup(&key_hash(&raw)).unwrap().expect("issued key is found by its hash");
    assert_eq!(got, meta);
    assert_eq!(ks.lookup(&key_hash("ks_0000")).unwrap(), None);
    assert_eq!(ks.list().unwrap(), vec![meta.clone()]);
    assert_eq!((ks.count_active(Role::Admin).unwrap(), ks.count_active(Role::Owner).unwrap()), (1, 0));

    assert!(ks.revoke(&meta.id, "2026-11-12T01:00:00Z").unwrap());
    assert!(!ks.revoke("ffffffffffffffff", "x").unwrap(), "unknown id");
    let got = ks.lookup(&key_hash(&raw)).unwrap().unwrap();
    assert_eq!(got.revoked_at.as_deref(), Some("2026-11-12T01:00:00Z"));
    assert!(!got.active());
    assert_eq!(ks.count_active(Role::Reader).unwrap(), 0, "a revoked key does not count");

    // the same lookup through the server-side store (its own connection)
    let store = RedisAuthStore::new(&url, &prefix);
    assert_eq!(store.lookup(&key_hash(&raw)).unwrap().map(|m| m.id), Some(meta.id.clone()));
    assert!(RedisAuthStore::new("redis://127.0.0.1:1", &prefix).lookup(&key_hash(&raw)).is_err(), "an unreachable store is an error, not 'no key'");
}

#[test]
fn issue_rejects_empty_or_oversized_metadata() {
    let Some((url, prefix, _purge)) = redis() else { return };
    let mut ks = KeyStore::connect(&url, &prefix).unwrap();
    for (p, u) in [("", "alice"), ("demo", " "), (&"x".repeat(257)[..], "alice"), ("de\nmo", "alice")] {
        assert!(ks.issue(Role::Reader, p, u, "t").is_err(), "{p:?} {u:?}");
    }
    assert_eq!(ks.list().unwrap(), vec![]);
}

#[test]
fn keys_survive_an_index_rebuild() {
    let Some((url, prefix, _purge)) = redis() else { return };
    let mut ks = KeyStore::connect(&url, &prefix).unwrap();
    let (meta, raw) = ks.issue(Role::Owner, "personal", "owner", "t").unwrap();
    let vocab = Vocab::from_json(r#"{"version":1,"terms":[{"term":"redis","facet":"entity","aliases":[]}]}"#, "t").unwrap();
    let mut c = Card::new("k-1");
    c.title = Some("T".into());
    let fp = Fingerprints { vocab_fp: "v".into(), cards_fp: "c".into() };
    let mut idx = Index::connect(&url, &prefix).unwrap();
    idx.rebuild_with(&[c], &vocab, None, &fp).unwrap();
    idx.rebuild_with(&[], &vocab, None, &fp).unwrap();
    assert_eq!(ks.lookup(&key_hash(&raw)).unwrap(), Some(meta));
}
