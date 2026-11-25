//! The verbatim Lua scripts of the original (SPEC-1 §3.4), wrapped in [`redis::Script`]
//! (EVALSHA with an EVAL fallback).

use std::sync::LazyLock;

use redis::Script;

/// `setKeepTTL` – 1 key, ARGV `[value]`: SET preserving the remaining TTL.
pub const SET_KEEP_TTL_LUA: &str = r#"local ttl = redis.call("pttl", KEYS[1]) if ttl > 0 then return redis.call("SET", KEYS[1], ARGV[1], "PX", ttl) else return redis.call("SET", KEYS[1], ARGV[1]) end"#;

/// `lremindex` – 1 key, ARGV `[index]`: remove a list element by index.
pub const LREM_INDEX_LUA: &str = r#"local FLAG = "$$#__@DELETE@_REDIS_@PRO@__#$$" redis.call("lset", KEYS[1], ARGV[1], FLAG) redis.call("lrem", KEYS[1], 1, FLAG)"#;

/// `duplicateKey` – 2 keys, ARGV `['TTL' | 'NOTTL']`: DUMP + RESTORE (verbatim from the original).
///
/// With `TTL` and a persistent source key `pttl` is `-1`, which RESTORE rejects; the port
/// therefore invokes [`DUPLICATE_KEY_FIXED_LUA`] (see [`duplicate_key`]). This constant is kept
/// for reference/tests.
pub const DUPLICATE_KEY_LUA: &str = r#"local dump = redis.call("dump", KEYS[1]) local pttl = 0 if ARGV[1] == "TTL" then pttl = redis.call("pttl", KEYS[1]) end return redis.call("restore", KEYS[2], pttl, dump)"#;

/// `duplicateKey` with the SPEC-2 port fix: use 0 when `pttl < 0`.
pub const DUPLICATE_KEY_FIXED_LUA: &str = r#"local dump = redis.call("dump", KEYS[1]) local pttl = 0 if ARGV[1] == "TTL" then pttl = redis.call("pttl", KEYS[1]) if pttl < 0 then pttl = 0 end end return redis.call("restore", KEYS[2], pttl, dump)"#;

static SET_KEEP_TTL: LazyLock<Script> = LazyLock::new(|| Script::new(SET_KEEP_TTL_LUA));
static LREM_INDEX: LazyLock<Script> = LazyLock::new(|| Script::new(LREM_INDEX_LUA));
static DUPLICATE_KEY: LazyLock<Script> = LazyLock::new(|| Script::new(DUPLICATE_KEY_FIXED_LUA));

/// The `setKeepTTL` script.
pub fn set_keep_ttl() -> &'static Script {
    &SET_KEEP_TTL
}

/// The `lremindex` script.
pub fn lrem_index() -> &'static Script {
    &LREM_INDEX
}

/// The `duplicateKey` script (with the `pttl < 0 -> 0` fix).
pub fn duplicate_key() -> &'static Script {
    &DUPLICATE_KEY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_have_hashes() {
        assert_eq!(set_keep_ttl().get_hash().len(), 40);
        assert_eq!(lrem_index().get_hash().len(), 40);
        assert_eq!(duplicate_key().get_hash().len(), 40);
        assert_ne!(DUPLICATE_KEY_LUA, DUPLICATE_KEY_FIXED_LUA);
    }
}
