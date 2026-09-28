//! 用户实体（兼容 legacy User / users.json 全字段，JSON 字段 snake_case 与 legacy 一致）

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Default, Serialize, Deserialize, FromRow)]
#[serde(default)]
pub struct User {
    pub username: String,
    pub password: String,
    pub salt: String,
    pub token: String,
    /// 多会话 token → 过期时间（legacy token_map）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_map: Option<serde_json::Value>,
    pub enable_webdav: bool,
    pub enable_local_store: bool,
    pub enable_book_source: bool,
    pub enable_rss_source: bool,
    pub book_source_limit: i64,
    pub book_limit: i64,
    /// 管理员（secure 模式下可修改 default 系统配置；首个注册用户自动成为管理员）
    #[serde(default)]
    pub is_admin: bool,
    /// 账号停用：登录与既有 token 一律拒绝（管理员可在用户管理页启停）
    #[serde(default)]
    pub disabled: bool,
    pub last_login_at: i64,
    pub created_at: i64,
    #[serde(skip)]
    #[sqlx(rename = "user_namespace")]
    pub user_namespace: String,
    /// 迁移保底：原始 JSON 全量
    #[serde(skip)]
    #[sqlx(rename = "raw_json")]
    pub raw_json: Option<String>,
}

// ==================== GAP 59 多设备 token（users.token_map） ====================
//
// token_map 存储为 JSON 对象 Map<token, 过期毫秒时间戳>（与 legacy 完全一致）；
// 兼容旧数据形态（JSON 数组——纯 token 列表，无过期信息，视为长期有效）。

/// 会话 token 上限（GAP 59：多设备登录最多 5 个有效 token）
pub const MAX_USER_TOKENS: usize = 5;

/// 解析 token_map → token 列表（None / 非法 JSON / 空 → 空列表）
pub fn token_map_list(token_map: &Option<serde_json::Value>) -> Vec<String> {
    let Some(v) = token_map else { return vec![] };
    match v {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect(),
        // legacy 形态：{token: 过期时间戳}——键即 token
        serde_json::Value::Object(map) => map.keys().cloned().collect(),
        _ => vec![],
    }
}

/// token_map 是否包含指定 token（GAP 59：resolve_namespace 任一 token 均可通过）
pub fn token_map_contains(token_map: &Option<serde_json::Value>, token: &str) -> bool {
    token_map_list(token_map).iter().any(|t| t == token)
}

/// token 是否有效（含过期检查）：对象形态按过期时间戳判定（已过期 → false）；
/// 数组形态（旧数据）无过期信息 → 视为有效。
pub fn token_map_valid(token_map: &Option<serde_json::Value>, token: &str, now_ms: i64) -> bool {
    let Some(v) = token_map else { return false };
    match v {
        serde_json::Value::Object(map) => match map.get(token) {
            Some(serde_json::Value::Number(n)) => {
                n.as_i64().map(|exp| exp > now_ms).unwrap_or(false)
            }
            // 无过期信息（非数字值/手改数据）→ 视为有效
            Some(_) => true,
            None => false,
        },
        _ => token_map_contains(token_map, token),
    }
}

/// 追加 token（去重；带过期时间戳；清理已过期项；超出上限丢最旧），
/// 返回新 token_map JSON 字符串（legacy 对象形态 Map<token, expire_ms>）。
/// expire_ms 为登录时生成的过期时间戳（ttl<=0 永不过期 → i64::MAX）。
pub fn token_map_push(
    token_map: &Option<serde_json::Value>,
    token: &str,
    expire_ms: i64,
    now_ms: i64,
) -> String {
    // 用 Vec 显式管理顺序（不依赖 serde_json::Map 内部顺序，裁剪语义可预测）
    let mut entries: Vec<(String, i64)> = match token_map {
        Some(serde_json::Value::Object(m)) => m
            .iter()
            .filter_map(|(k, v)| v.as_i64().map(|exp| (k.clone(), exp)))
            .collect(),
        // 旧数组形态：转为对象，无过期信息 → 按主 token 同规则（expire_ms）续期
        _ => token_map_list(token_map)
            .into_iter()
            .map(|t| (t, expire_ms))
            .collect(),
    };
    // 清理已过期 token（legacy：tokenMap.values.removeAll { it < now }）
    entries.retain(|(_, exp)| *exp > now_ms);
    // 去重更新 / 追加本 token
    if let Some(e) = entries.iter_mut().find(|(k, _)| *k == token) {
        e.1 = expire_ms;
    } else {
        entries.push((token.to_string(), expire_ms));
    }
    // 上限 5：按过期时间升序丢最旧（相等时保留先插入者，避免误丢最新 token）
    while entries.len() > MAX_USER_TOKENS {
        let min_idx = entries
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, exp))| *exp)
            .map(|(i, _)| i)
            .unwrap_or(0);
        entries.remove(min_idx);
    }
    let map: serde_json::Map<String, serde_json::Value> = entries
        .into_iter()
        .map(|(k, exp)| (k, serde_json::json!(exp)))
        .collect();
    serde_json::to_string(&serde_json::Value::Object(map))
        .unwrap_or_else(|_| serde_json::json!({}).to_string())
}

/// 移除 token，返回 (移除后 token_map JSON, 是否确实移除了该 token)
pub fn token_map_remove(token_map: &Option<serde_json::Value>, token: &str) -> (String, bool) {
    let mut entries: Vec<(String, i64)> = match token_map {
        Some(serde_json::Value::Object(m)) => m
            .iter()
            .filter_map(|(k, v)| v.as_i64().map(|exp| (k.clone(), exp)))
            .collect(),
        _ => token_map_list(token_map)
            .into_iter()
            .map(|t| (t, 0))
            .collect(),
    };
    let before = entries.len();
    entries.retain(|(k, _)| *k != token);
    let removed = entries.len() != before;
    let map: serde_json::Map<String, serde_json::Value> = entries
        .into_iter()
        .map(|(k, exp)| (k, serde_json::json!(exp)))
        .collect();
    let json = serde_json::to_string(&serde_json::Value::Object(map))
        .unwrap_or_else(|_| serde_json::json!({}).to_string());
    (json, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_token_map_list_formats() {
        // 数组形态（旧数据兼容）
        let m = Some(json!(["t1", "t2"]));
        assert_eq!(token_map_list(&m), vec!["t1", "t2"]);
        // legacy 对象形态（键为 token）
        let m = Some(json!({"t1": 1700000000000i64, "t2": 1700000001000i64}));
        let mut list = token_map_list(&m);
        list.sort();
        assert_eq!(list, vec!["t1", "t2"]);
        // None / 非法
        assert!(token_map_list(&None).is_empty());
        assert!(token_map_list(&Some(json!("x"))).is_empty());
    }

    #[test]
    fn test_token_map_push_dedup_and_cap() {
        let now = 1_700_000_000_000i64;
        let mut m: Option<serde_json::Value> = None;
        for i in 0..7 {
            m = Some(
                serde_json::from_str(&token_map_push(
                    &m,
                    &format!("t{i}"),
                    now + 86_400_000 * 7,
                    now,
                ))
                .unwrap(),
            );
        }
        let list = token_map_list(&m);
        assert_eq!(list.len(), MAX_USER_TOKENS, "上限 5");
        assert_eq!(list, vec!["t2", "t3", "t4", "t5", "t6"], "最旧被丢");
        // 去重：重新推入已存在的 token 不重复
        let m = Some(json!({"t2": now + 1, "t3": now + 1}));
        let m2 = serde_json::from_str::<serde_json::Value>(&token_map_push(&m, "t3", now + 2, now))
            .unwrap();
        assert_eq!(token_map_list(&Some(m2)), vec!["t2", "t3"]);
    }

    #[test]
    fn test_token_map_contains_and_remove() {
        let m = Some(json!({"t1": 1, "t2": 2}));
        assert!(token_map_contains(&m, "t1"));
        assert!(token_map_contains(&m, "t2"));
        assert!(!token_map_contains(&m, "t3"));
        assert!(!token_map_contains(&None, ""));
        let (json, removed) = token_map_remove(&m, "t1");
        assert!(removed);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(token_map_list(&Some(v)), vec!["t2"]);
        let (json, removed) = token_map_remove(&Some(json!({"t1": 1})), "nope");
        assert!(!removed);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(token_map_list(&Some(v)), vec!["t1"]);
    }

    #[test]
    fn test_token_map_valid_expiry() {
        let now = 1_700_000_000_000i64;
        let m = Some(json!({"fresh": now + 1000, "stale": now - 1000, "no_exp": "x"}));
        assert!(token_map_valid(&m, "fresh", now));
        assert!(!token_map_valid(&m, "stale", now), "过期 token 失效");
        assert!(token_map_valid(&m, "no_exp", now), "无过期信息 → 视为有效");
        assert!(!token_map_valid(&m, "missing", now));
        // 旧数组形态：无过期信息 → 视为有效
        assert!(token_map_valid(&Some(json!(["t1"])), "t1", now));
    }

    #[test]
    fn test_token_map_push_prunes_expired() {
        let now = 1_700_000_000_000i64;
        let m = Some(json!({"stale": now - 1, "fresh": now + 1000}));
        let m2 = serde_json::from_str::<serde_json::Value>(&token_map_push(
            &m,
            "new_tok",
            now + 1000,
            now,
        ))
        .unwrap();
        let list = token_map_list(&Some(m2));
        assert_eq!(list, vec!["fresh", "new_tok"], "过期项被清理");
    }
}
