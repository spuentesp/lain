//! §10.5 paging and limits for contract tools.

use blake3::Hasher;
use serde_json::{json, Map, Value};

/// Stable hash of the argument map (excluding the `cursor`) used as
/// the `q` half of a cursor token. `cursor_mismatch` is raised when
/// the same cursor is reused with different arguments (`§10.5`).
///
/// We canonicalize via `serde_json` (which is deterministic for the
/// shapes we accept — sorted `BTreeMap` args and flat `Value`).
pub fn fingerprint(args: &Map<String, Value>) -> String {
    let mut filtered: Map<String, Value> = Map::new();
    for (k, v) in args {
        if k == "cursor" {
            continue;
        }
        filtered.insert(k.clone(), v.clone());
    }
    let canonical = serde_json::to_vec(&Value::Object(filtered)).unwrap_or_default();
    let mut h = Hasher::new();
    h.update(&canonical);
    let digest = h.finalize();
    let hex = digest.to_hex();
    hex.to_string().chars().take(8).collect()
}

/// Encode a cursor: `base64url({"v":1, "after": <sort_key>, "q": <hash>})`
/// without padding. We implement the encoder inline (RFC 4648 §5) to
/// avoid pulling `base64` as a direct dependency — the project's
/// Cargo.toml policy forbids new transitive deps until the maintainer
/// approves (`docs/CONTRIBUTING_AGENTS.md`).
pub fn encode_cursor(after: &str, fingerprint_hex: &str) -> String {
    let payload = json!({ "v": 1, "after": after, "q": fingerprint_hex });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    base64url_encode(&bytes)
}

/// Decode a cursor. Returns `Err(reason)` when the token is malformed
/// or its version is unsupported.
pub fn decode_cursor(token: &str) -> Result<CursorPayload, String> {
    let bytes = base64url_decode(token).map_err(|_| "malformed cursor".to_string())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| "malformed cursor".to_string())?;
    let after = v
        .get("after")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "missing after".to_string())?
        .to_string();
    let q = v
        .get("q")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "missing q".to_string())?
        .to_string();
    let version = v.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
    if version != 1 {
        return Err(format!("unsupported cursor version {version}"));
    }
    Ok(CursorPayload { after, q })
}

/// The decoded cursor.
#[derive(Debug, Clone)]
pub struct CursorPayload {
    pub after: String,
    pub q: String,
}

/// Apply a cursor to a slice: return items whose sort key is strictly
/// greater than the cursor's `after`. Used by both `list_services` and
/// `get_service`'s consumer paging.
pub fn apply_cursor<T, F>(items: &[T], cursor: Option<&CursorPayload>, key: F) -> Vec<T>
where
    T: Clone,
    F: Fn(&T) -> String,
{
    let Some(c) = cursor else {
        return items.to_vec();
    };
    items
        .iter()
        .filter(|it| key(it) > c.after)
        .cloned()
        .collect()
}

/// Apply a `limit` cap, returning `(page, next_cursor)`. The `last_key`
/// argument is the sort key of the last returned item; it's encoded
/// into the next cursor when the page is full.
pub fn apply_limit<T, F>(items: Vec<T>, limit: usize, key: F) -> (Vec<T>, Option<String>)
where
    F: Fn(&T) -> String,
{
    if items.len() <= limit {
        return (items, None);
    }
    let mut page = items;
    let next_key = page[limit.saturating_sub(1)..]
        .first()
        .map(key)
        .unwrap_or_default();
    page.truncate(limit);
    let cursor = encode_cursor(&next_key, "");
    (page, Some(cursor))
}

// ─── inline base64url (no `=` padding) ──────────────────────────────────

const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(B64_URL[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_URL[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64_URL[((n >> 6) & 0x3F) as usize] as char);
        out.push(B64_URL[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(B64_URL[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_URL[((n >> 12) & 0x3F) as usize] as char);
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(B64_URL[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_URL[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64_URL[((n >> 6) & 0x3F) as usize] as char);
    }
    out
}

fn base64url_decode(input: &str) -> Result<Vec<u8>, ()> {
    let mut lookup = [255u8; 256];
    for (i, &b) in B64_URL.iter().enumerate() {
        lookup[b as usize] = i as u8;
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        let v = lookup[b as usize];
        if v == 255 {
            return Err(());
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
            buf &= (1u32 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_stable_across_calls() {
        let mut a = Map::new();
        a.insert("x".to_string(), json!(1));
        a.insert("y".to_string(), json!("foo"));
        let mut b = Map::new();
        b.insert("y".to_string(), json!("foo"));
        b.insert("x".to_string(), json!(1));
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn fingerprint_changes_when_args_change() {
        let mut a = Map::new();
        a.insert("x".to_string(), json!(1));
        let mut b = Map::new();
        b.insert("x".to_string(), json!(2));
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn fingerprint_ignores_cursor() {
        let mut a = Map::new();
        a.insert("x".to_string(), json!(1));
        a.insert("cursor".to_string(), json!("token"));
        let mut b = Map::new();
        b.insert("x".to_string(), json!(1));
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn cursor_roundtrip() {
        let token = encode_cursor("orders", "abcdef01");
        let decoded = decode_cursor(&token).unwrap();
        assert_eq!(decoded.after, "orders");
        assert_eq!(decoded.q, "abcdef01");
    }

    #[test]
    fn cursor_rejects_garbage() {
        assert!(decode_cursor("@@@").is_err());
    }

    #[test]
    fn apply_cursor_filters_strictly_greater() {
        let items = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let c = CursorPayload {
            after: "alpha".to_string(),
            q: "x".to_string(),
        };
        let page = apply_cursor(&items, Some(&c), |s| s.clone());
        assert_eq!(page, vec!["beta".to_string(), "gamma".to_string()]);
    }

    #[test]
    fn apply_limit_emits_next_cursor_when_full() {
        let items: Vec<String> = (0..10).map(|i| format!("item-{:02}", i)).collect();
        let (page, next) = apply_limit(items, 4, |s| s.clone());
        assert_eq!(page.len(), 4);
        assert!(next.is_some());
    }

    #[test]
    fn apply_limit_omits_cursor_when_page_partial() {
        let items: Vec<String> = (0..3).map(|i| format!("item-{:02}", i)).collect();
        let (page, next) = apply_limit(items, 100, |s| s.clone());
        assert_eq!(page.len(), 3);
        assert!(next.is_none());
    }

    #[test]
    fn base64url_roundtrip() {
        let bytes = b"{\"v\":1,\"after\":\"orders\",\"q\":\"abcdef01\"}";
        let s = base64url_encode(bytes);
        assert!(!s.contains('='));
        let back = base64url_decode(&s).unwrap();
        assert_eq!(back, bytes);
    }
}
