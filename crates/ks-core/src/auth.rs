//! Operator-issued API keys for the remote MCP server (`ks mcp serve --http`).
//!
//! A key is `ks_` + 64 hex characters (32 random bytes). It is shown once, to the operator who issues it, and never
//! stored: Redis keeps only its SHA-256 and the metadata, under the same key prefix as the card index:
//! - `<prefix>auth:key:<sha256 hex>`: hash with `id`, `role`, `purpose`, `user`, `created_at` and, once revoked,
//!   `revoked_at`;
//! - `<prefix>auth:id:<id>`: the sha256 hex, so `ks key revoke <id>` finds the key.
//!
//! An index rebuild deletes only `meta` and `card:*`, so keys survive it. Advanced features (OAuth, rate limits, an
//! audit log, TLS) are out of scope.
use crate::index::{env_url_and_prefix, glob_escape, rerr};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

pub const KEY_PREFIX: &str = "ks_";
const MAX_META_CHARS: usize = 256;

/// What a key may see: readers get public cards with source fields removed; admins and owners get everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Reader,
    Admin,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Reader => "reader",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "reader" => Some(Role::Reader),
            "admin" => Some(Role::Admin),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
}

/// The stored metadata of one key. Never holds the key or its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyMeta {
    pub id: String,
    pub role: Role,
    pub purpose: String,
    pub user: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

impl KeyMeta {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"id": self.id, "role": self.role.as_str(), "purpose": self.purpose, "user": self.user, "created_at": self.created_at, "revoked_at": self.revoked_at})
    }
}

fn random_hex(n: usize) -> Result<String> {
    use std::io::Read;
    let mut b = vec![0u8; n];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).map_err(|e| Error::msg(format!("/dev/urandom: {e}")))?;
    Ok(hex::encode(b))
}

/// A fresh key: `ks_` + 32 random bytes as hex.
pub fn new_key() -> Result<String> {
    Ok(format!("{KEY_PREFIX}{}", random_hex(32)?))
}

/// SHA-256 of the key, as lowercase hex. This is all that is stored.
pub fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

fn check_meta(what: &str, v: &str) -> Result<()> {
    if v.trim().is_empty() || v.chars().count() > MAX_META_CHARS || v.chars().any(char::is_control) {
        return Err(Error::msg(format!("{what} must be 1-{MAX_META_CHARS} characters without control characters")));
    }
    Ok(())
}

/// Looks up a presented key by its hash. `Err` means the store could not answer (the server answers 503, never "no key").
pub trait AuthStore: Send + Sync {
    fn lookup(&self, hash: &str) -> Result<Option<KeyMeta>>;
}

/// Key operations on one Redis connection.
pub struct KeyStore {
    con: redis::Connection,
    prefix: String,
}

impl KeyStore {
    pub fn connect(url: &str, prefix: &str) -> Result<KeyStore> {
        if prefix.is_empty() {
            return Err(Error::msg("redis key prefix must not be empty"));
        }
        let client = redis::Client::open(url).map_err(rerr)?;
        let con = client.get_connection_with_timeout(Duration::from_secs(5)).map_err(rerr)?;
        con.set_read_timeout(Some(Duration::from_secs(10))).map_err(rerr)?;
        Ok(KeyStore { con, prefix: prefix.to_string() })
    }

    /// Same Redis and key prefix as `Index::from_env`.
    pub fn from_env() -> Result<KeyStore> {
        let (url, prefix) = env_url_and_prefix()?;
        KeyStore::connect(&url, &prefix)
    }

    fn key_of(&self, hash: &str) -> String {
        format!("{}auth:key:{hash}", self.prefix)
    }
    fn id_of(&self, id: &str) -> String {
        format!("{}auth:id:{id}", self.prefix)
    }

    /// Store a new key and return its metadata and the key itself (the only time it exists outside the caller).
    pub fn issue(&mut self, role: Role, purpose: &str, user: &str, now: &str) -> Result<(KeyMeta, String)> {
        check_meta("purpose", purpose)?;
        check_meta("user", user)?;
        let (key, id) = (new_key()?, random_hex(8)?);
        let hash = key_hash(&key);
        let (k, i) = (self.key_of(&hash), self.id_of(&id));
        let fields = [("id", id.as_str()), ("role", role.as_str()), ("purpose", purpose), ("user", user), ("created_at", now)];
        // both keys at once, and only if neither exists (a collision of 64-bit ids or 256-bit keys is refused, not overwritten)
        let script = redis::Script::new(
            "if redis.call('EXISTS', KEYS[1]) == 1 or redis.call('EXISTS', KEYS[2]) == 1 then return 0 end \
             redis.call('HSET', KEYS[1], unpack(ARGV, 2)) redis.call('SET', KEYS[2], ARGV[1]) return 1",
        );
        let mut inv = script.key(&k);
        inv.key(&i).arg(&hash);
        for (f, v) in fields {
            inv.arg(f).arg(v);
        }
        let n: i64 = inv.invoke(&mut self.con).map_err(rerr)?;
        if n != 1 {
            return Err(Error::msg("key id collision; issue the key again"));
        }
        let meta = KeyMeta { id, role, purpose: purpose.into(), user: user.into(), created_at: now.into(), revoked_at: None };
        Ok((meta, key))
    }

    pub fn lookup(&mut self, hash: &str) -> Result<Option<KeyMeta>> {
        let k = self.key_of(hash);
        let h: HashMap<String, String> = redis::cmd("HGETALL").arg(&k).query(&mut self.con).map_err(rerr)?;
        if h.is_empty() {
            return Ok(None);
        }
        parse_meta(&h).map(Some)
    }

    /// Every key's metadata, sorted by creation time then id.
    pub fn list(&mut self) -> Result<Vec<KeyMeta>> {
        let pat = format!("{}auth:key:*", glob_escape(&self.prefix));
        let keys: Vec<String> = {
            let iter: redis::Iter<String> = redis::cmd("SCAN").cursor_arg(0).arg("MATCH").arg(&pat).arg("COUNT").arg(1000).clone().iter(&mut self.con).map_err(rerr)?;
            iter.collect::<std::result::Result<_, _>>().map_err(rerr)?
        };
        let mut out = vec![];
        for k in keys {
            let h: HashMap<String, String> = redis::cmd("HGETALL").arg(&k).query(&mut self.con).map_err(rerr)?;
            if !h.is_empty() {
                out.push(parse_meta(&h)?);
            }
        }
        out.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        Ok(out)
    }

    /// Mark the key revoked (effective on the next request). Returns false for an unknown id. Revoking twice keeps the
    /// first time.
    pub fn revoke(&mut self, id: &str, now: &str) -> Result<bool> {
        let hash: Option<String> = redis::cmd("GET").arg(self.id_of(id)).query(&mut self.con).map_err(rerr)?;
        let Some(hash) = hash else { return Ok(false) };
        let k = self.key_of(&hash);
        let n: i64 = redis::Script::new("if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end redis.call('HSETNX', KEYS[1], 'revoked_at', ARGV[1]) return 1")
            .key(&k)
            .arg(now)
            .invoke(&mut self.con)
            .map_err(rerr)?;
        Ok(n == 1)
    }

    /// Keys that are not revoked and have at least role `min`.
    pub fn count_active(&mut self, min: Role) -> Result<usize> {
        Ok(self.list()?.into_iter().filter(|m| m.active() && m.role >= min).count())
    }
}

fn parse_meta(h: &HashMap<String, String>) -> Result<KeyMeta> {
    let get = |f: &str| h.get(f).cloned().ok_or_else(|| Error::msg(format!("stored key without {f}")));
    let role = get("role")?;
    Ok(KeyMeta {
        id: get("id")?,
        role: Role::parse(&role).ok_or_else(|| Error::msg(format!("stored key with unknown role {role:?}")))?,
        purpose: get("purpose")?,
        user: get("user")?,
        created_at: get("created_at")?,
        revoked_at: h.get("revoked_at").cloned(),
    })
}

/// The server's `AuthStore`: its own Redis connection (not the search connection), connected lazily and dropped
/// after an error so the next request reconnects. Lookups are serialized; a pool is out of scope.
pub struct RedisAuthStore {
    url: String,
    prefix: String,
    con: Mutex<Option<KeyStore>>,
}

impl RedisAuthStore {
    pub fn new(url: &str, prefix: &str) -> RedisAuthStore {
        RedisAuthStore { url: url.into(), prefix: prefix.into(), con: Mutex::new(None) }
    }
    pub fn from_env() -> Result<RedisAuthStore> {
        let (url, prefix) = env_url_and_prefix()?;
        Ok(RedisAuthStore::new(&url, &prefix))
    }
    fn with<T>(&self, f: impl FnOnce(&mut KeyStore) -> Result<T>) -> Result<T> {
        let mut g = self.con.lock().map_err(|_| Error::msg("auth store lock poisoned"))?;
        if g.is_none() {
            *g = Some(KeyStore::connect(&self.url, &self.prefix)?);
        }
        let r = f(g.as_mut().expect("connected above"));
        if r.is_err() {
            *g = None;
        }
        r
    }
    pub fn count_active(&self, min: Role) -> Result<usize> {
        self.with(|k| k.count_active(min))
    }
}

impl AuthStore for RedisAuthStore {
    fn lookup(&self, hash: &str) -> Result<Option<KeyMeta>> {
        self.with(|k| k.lookup(hash))
    }
}
