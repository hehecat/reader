//! 书源实体（兼容 legacy BookSource / bookSource.json 全字段）
//!
//! - 规则字段（ruleSearch 等）为嵌套 JSON 对象 → `Option<serde_json::Value>`（存文本/原样输出）
//! - 序列化：字段名与 legacy bookSource.json 一致（camelCase）
//! - raw_json 保底：未知字段不丢

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// legacy/源仓库 JSON 的数字字段常以字符串下发(如 lastUpdateTime: "1700000000000"),
/// 严格 i64 反序列化会整源拒收 → 容忍 数字/数字字符串/null
fn lenient_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| D::Error::custom("invalid number")),
        serde_json::Value::String(t) => t
            .trim()
            .parse::<i64>()
            .or_else(|_| t.trim().parse::<f64>().map(|f| f as i64))
            .map_err(|_| D::Error::custom("invalid number string")),
        serde_json::Value::Null => Ok(0),
        _ => Err(D::Error::custom("invalid number")),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, FromRow)]
#[serde(default)]
pub struct BookSource {
    #[serde(rename = "bookSourceUrl")]
    #[sqlx(rename = "book_source_url")]
    pub book_source_url: String,
    #[serde(rename = "bookSourceName")]
    #[sqlx(rename = "book_source_name")]
    pub book_source_name: String,
    #[serde(rename = "bookSourceGroup")]
    #[sqlx(rename = "book_source_group")]
    pub book_source_group: Option<String>,
    #[serde(rename = "bookSourceType")]
    #[sqlx(rename = "book_source_type")]
    #[serde(deserialize_with = "lenient_i64")]
    pub book_source_type: i64,
    #[serde(rename = "bookUrlPattern")]
    #[sqlx(rename = "book_url_pattern")]
    pub book_url_pattern: Option<String>,
    #[serde(rename = "customOrder")]
    #[sqlx(rename = "custom_order")]
    #[serde(deserialize_with = "lenient_i64")]
    pub custom_order: i64,
    pub enabled: bool,
    #[serde(rename = "enabledExplore")]
    #[sqlx(rename = "enabled_explore")]
    pub enabled_explore: bool,
    #[serde(rename = "enabledCookieJar")]
    #[sqlx(rename = "enabled_cookie_jar")]
    pub enabled_cookie_jar: Option<bool>,
    #[serde(rename = "concurrentRate")]
    #[sqlx(rename = "concurrent_rate")]
    pub concurrent_rate: Option<String>,
    /// JS 依赖库（legacy jsLib——书源共享 JS 作用域，定义 AES_KEY/sign 等全局常量）
    #[serde(rename = "jsLib")]
    #[sqlx(rename = "js_lib")]
    pub js_lib: Option<String>,
    pub header: Option<String>,
    /// 书源级代理（如 socks5://127.0.0.1:1080）——CF 质询/Turnstile 求解时透传
    /// camoufox 代理（浏览器流量走代理；书源直连抓取不受影响）。
    /// 未配置时回退环境变量 READER_CAMOUFOX_PROXY
    #[serde(rename = "proxyUrl")]
    #[sqlx(rename = "proxy_url")]
    pub proxy_url: Option<String>,
    #[serde(rename = "loginUrl")]
    #[sqlx(rename = "login_url")]
    pub login_url: Option<String>,
    #[serde(rename = "loginUi")]
    #[sqlx(rename = "login_ui")]
    pub login_ui: Option<String>,
    #[serde(rename = "loginCheckJs")]
    #[sqlx(rename = "login_check_js")]
    pub login_check_js: Option<String>,
    #[serde(rename = "loginJs")]
    #[sqlx(rename = "login_js")]
    pub login_js: Option<String>,
    #[serde(rename = "bookSourceComment")]
    #[sqlx(rename = "book_source_comment")]
    pub book_source_comment: Option<String>,
    #[serde(rename = "variableComment")]
    #[sqlx(rename = "variable_comment")]
    pub variable_comment: Option<String>,
    #[serde(rename = "lastUpdateTime")]
    #[sqlx(rename = "last_update_time")]
    #[serde(deserialize_with = "lenient_i64")]
    pub last_update_time: i64,
    #[serde(rename = "respondTime")]
    #[sqlx(rename = "respond_time")]
    #[serde(deserialize_with = "lenient_i64")]
    pub respond_time: i64,
    #[serde(deserialize_with = "lenient_i64")]
    pub weight: i64,
    // ---- 使用统计（权重自动调整数据源；serde skip：不外泄/不参与 raw_json，
    //      客户端回写 saveBookSource 也不会覆盖——upsert 不写这两列）----
    #[serde(skip)]
    #[sqlx(rename = "use_count")]
    pub use_count: i64,
    #[serde(skip)]
    #[sqlx(rename = "use_ts")]
    pub use_ts: i64,
    #[serde(rename = "exploreUrl")]
    #[sqlx(rename = "explore_url")]
    pub explore_url: Option<String>,
    #[serde(rename = "searchUrl")]
    #[sqlx(rename = "search_url")]
    pub search_url: Option<String>,
    // ---- 规则（legacy + legado 两套命名，均为嵌套对象）----
    #[serde(rename = "ruleExplore")]
    #[sqlx(rename = "rule_explore")]
    pub rule_explore: Option<serde_json::Value>,
    #[serde(rename = "ruleSearch")]
    #[sqlx(rename = "rule_search")]
    pub rule_search: Option<serde_json::Value>,
    #[serde(rename = "ruleBookInfo")]
    #[sqlx(rename = "rule_book_info")]
    pub rule_book_info: Option<serde_json::Value>,
    #[serde(rename = "ruleToc")]
    #[sqlx(rename = "rule_toc")]
    pub rule_toc: Option<serde_json::Value>,
    #[serde(rename = "ruleContent")]
    #[sqlx(rename = "rule_content")]
    pub rule_content: Option<serde_json::Value>,
    #[serde(rename = "ruleRelated")]
    #[sqlx(rename = "rule_related")]
    pub rule_related: Option<serde_json::Value>,
    #[serde(rename = "searchRule")]
    #[sqlx(rename = "search_rule")]
    pub search_rule: Option<serde_json::Value>,
    #[serde(rename = "exploreRule")]
    #[sqlx(rename = "explore_rule")]
    pub explore_rule: Option<serde_json::Value>,
    #[serde(rename = "bookInfoRule")]
    #[sqlx(rename = "book_info_rule")]
    pub book_info_rule: Option<serde_json::Value>,
    #[serde(rename = "tocRule")]
    #[sqlx(rename = "toc_rule")]
    pub toc_rule: Option<serde_json::Value>,
    #[serde(rename = "contentRule")]
    #[sqlx(rename = "content_rule")]
    pub content_rule: Option<serde_json::Value>,
    // ---- legado 扩展 ----
    pub key: Option<String>,
    pub tag: Option<String>,
    pub logger: Option<serde_json::Value>,
    pub variable: Option<serde_json::Value>,
    #[serde(skip)]
    #[sqlx(rename = "user_namespace")]
    pub user_namespace: String,
    /// 用户私有“已删除”覆盖标记（普通用户删除 default 系统书源时复制到本人命名空间并隐藏）
    #[serde(skip)]
    #[sqlx(rename = "hidden")]
    pub hidden: bool,
    #[serde(skip)]
    #[sqlx(rename = "raw_json")]
    pub raw_json: Option<String>,
}

/// 兼容远程书源 JSON 三种形态：数组 / `{bookSourceList:[...]}` / 单个书源对象。
/// 数字与布尔字段做宽松类型归一（legacy 书源常以字符串表示数字/布尔），
/// 未知字段保留（serde 忽略），解析失败的单条跳过。
pub fn normalize_book_sources(value: serde_json::Value) -> Vec<BookSource> {
    let items: Vec<serde_json::Value> = match value {
        serde_json::Value::Array(arr) => arr,
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::Array(arr)) = obj.get("bookSourceList").cloned() {
                arr
            } else if obj.contains_key("bookSourceUrl") {
                vec![serde_json::Value::Object(obj)]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for item in items {
        let serde_json::Value::Object(mut m) = item else {
            continue;
        };
        let url = value_to_string(m.get("bookSourceUrl").cloned().unwrap_or_default());
        let url = url.trim().to_string();
        if url.is_empty() {
            continue;
        }
        let name = {
            let n = value_to_string(m.get("bookSourceName").cloned().unwrap_or_default());
            if n.trim().is_empty() {
                url.clone()
            } else {
                n.trim().to_string()
            }
        };
        m.insert("bookSourceUrl".into(), serde_json::Value::String(url));
        m.insert("bookSourceName".into(), serde_json::Value::String(name));
        for key in [
            "bookSourceType",
            "customOrder",
            "lastUpdateTime",
            "respondTime",
            "weight",
        ] {
            if let Some(v) = m.get(key).cloned() {
                m.insert(key.into(), serde_json::json!(value_to_i64(&v)));
            }
        }
        if let Some(v) = m.get("enabled").cloned() {
            m.insert("enabled".into(), serde_json::json!(value_to_bool(&v, true)));
        }
        if let Some(v) = m.get("enabledExplore").cloned() {
            m.insert(
                "enabledExplore".into(),
                serde_json::json!(value_to_bool(&v, false)),
            );
        }
        if let Some(v) = m.get("enabledCookieJar").cloned() {
            m.insert(
                "enabledCookieJar".into(),
                serde_json::json!(value_to_bool(&v, false)),
            );
        }
        if let Ok(s) = serde_json::from_value::<BookSource>(serde_json::Value::Object(m)) {
            out.push(s);
        }
    }
    out
}

fn value_to_string(v: serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn value_to_i64(v: &serde_json::Value) -> i64 {
    match v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        serde_json::Value::String(s) => s
            .trim()
            .parse::<i64>()
            .or_else(|_| s.trim().parse::<f64>().map(|f| f as i64))
            .unwrap_or(0),
        serde_json::Value::Bool(b) => i64::from(*b),
        _ => 0,
    }
}

fn value_to_bool(v: &serde_json::Value, default: bool) -> bool {
    match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(|i| i != 0)
            .or_else(|| n.as_f64().map(|f| f != 0.0))
            .unwrap_or(default),
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 远程订阅常见宽松类型（字符串数字/布尔、对象包裹、单对象）均能归一导入
    #[test]
    fn test_normalize_book_sources_lenient() {
        let raw = serde_json::json!([
            {
                "bookSourceUrl": "https://a.com/",
                "bookSourceName": "A源",
                "bookSourceType": "0",
                "customOrder": "3",
                "enabled": "true",
                "weight": "7"
            },
            {
                "bookSourceUrl": "https://b.com/",
                "bookSourceName": "B源",
                "bookSourceType": 1,
                "enabled": false
            }
        ]);
        let list = normalize_book_sources(raw);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].book_source_url, "https://a.com/");
        assert_eq!(list[0].book_source_type, 0);
        assert_eq!(list[0].custom_order, 3);
        assert!(list[0].enabled);
        assert_eq!(list[0].weight, 7);
        assert_eq!(list[1].book_source_type, 1);
        assert!(!list[1].enabled);

        let wrapped = serde_json::json!({
            "bookSourceList": [
                {"bookSourceUrl": "https://c.com/", "bookSourceName": "C源"}
            ]
        });
        assert_eq!(normalize_book_sources(wrapped).len(), 1);

        let single = serde_json::json!({
            "bookSourceUrl": "https://d.com/",
            "bookSourceName": "D源"
        });
        assert_eq!(normalize_book_sources(single).len(), 1);

        assert!(normalize_book_sources(serde_json::json!({ "x": 1 })).is_empty());
    }

    /// 源仓库/legacy 导出常把数字字段写成字符串 → 必须容忍, 否则整源拒收
    #[test]
    fn 数字字段容忍字符串与null() {
        let v = serde_json::json!({
            "bookSourceUrl": "https://e.com/",
            "bookSourceName": "E源",
            "lastUpdateTime": "1700000000000",
            "respondTime": "60000",
            "weight": null,
            "bookSourceType": "1",
            "customOrder": 3
        });
        let s: BookSource = serde_json::from_value(v).expect("字符串数字应可反序列化");
        assert_eq!(s.last_update_time, 1700000000000);
        assert_eq!(s.respond_time, 60000);
        assert_eq!(s.weight, 0);
        assert_eq!(s.book_source_type, 1);
        assert_eq!(s.custom_order, 3);
    }
}
