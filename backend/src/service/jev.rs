//! 判定模型「Jev / EMRank」的独立配置与连通性探测。
//!
//! Jev 不是 OpenAI 兼容的 chat 端点：它一次提交「状态 + 一组封闭式问题」，
//! 走 `POST {base_url}/decisions`，返回每个问题的答案（noul 概率 / 选项 / 分值）。
//! 端点、密钥、模型都和阅读助手那套分开存，互不影响。
//!
//! 目前只提供「配置 + 测试连接」：还没有业务调用方，先把端点摸清楚、能验通。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

use serde_json::{json, Value};

use crate::storage::Storage;

/// 设置页保存的判定模型配置（存 `system_settings` 键 `jev_settings`）
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct JevConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

pub const DEFAULT_BASE_URL: &str = "https://emrank.tech/v1";
pub const DEFAULT_MODEL: &str = "typesafe/jev-latest";

static RUNTIME: OnceLock<RwLock<Option<JevConfig>>> = OnceLock::new();
static LOADED: AtomicBool = AtomicBool::new(false);

fn runtime() -> &'static RwLock<Option<JevConfig>> {
    RUNTIME.get_or_init(|| RwLock::new(None))
}

fn write_runtime(cfg: Option<JevConfig>) {
    if let Ok(mut guard) = runtime().write() {
        *guard = cfg;
    }
}

/// 首次使用时从数据库载入（幂等）
pub async fn ensure_loaded(storage: &Storage) {
    if LOADED.swap(true, Ordering::Relaxed) {
        return;
    }
    match storage.get_system_setting("jev_settings").await {
        Ok(Some(raw)) => match serde_json::from_str::<JevConfig>(&raw) {
            Ok(cfg) => {
                tracing::info!("判定模型(Jev): 已载入设置页保存的端点配置");
                write_runtime(Some(cfg));
            }
            Err(e) => tracing::warn!("Jev 设置解析失败: {e}"),
        },
        Ok(None) => {}
        Err(e) => tracing::warn!("读取 Jev 设置失败: {e}"),
    }
}

/// 当前生效配置（未配置时为 None）
pub fn config() -> Option<JevConfig> {
    runtime().read().ok().and_then(|g| g.clone())
}

/// 保存并立即生效
pub async fn save(storage: &Storage, cfg: JevConfig) -> anyhow::Result<()> {
    let raw = serde_json::to_string(&cfg)?;
    storage.set_system_setting("jev_settings", &raw).await?;
    write_runtime(Some(cfg));
    LOADED.store(true, Ordering::Relaxed);
    Ok(())
}

/// 清空配置
pub async fn clear(storage: &Storage) -> anyhow::Result<()> {
    storage.delete_system_setting("jev_settings").await?;
    write_runtime(None);
    LOADED.store(true, Ordering::Relaxed);
    Ok(())
}

/// 探测结果：延迟 + 原始 answers（前端只展示几个字段）
pub struct Probe {
    pub latency_ms: u64,
    pub answers: Value,
}

/// 用一小组固定的封闭式问题探活：noul(是否) + choice(选项)
/// 状态与问题都与具体书籍无关，只为了验证端点/密钥/模型/协议能走通。
pub async fn probe(cfg: &JevConfig) -> anyhow::Result<Probe> {
    let body = json!({
        "model": cfg.model,
        "state": "赵玖下令处死康履, 康履已经死了。",
        "questions": {
            "dead": { "type": "noul", "instructions": "康履已经死了" },
            "who": {
                "type": "choice",
                "instructions": "谁下的令",
                "criteria": { "zhaojiu": "赵玖", "other": "其他人" }
            }
        }
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let started = std::time::Instant::now();
    let resp = client
        .post(format!("{}/decisions", cfg.base_url.trim_end_matches('/')))
        .bearer_auth(&cfg.api_key)
        .header(
            reqwest::header::USER_AGENT,
            // 该站点在 Cloudflare 后面: 没有浏览器 UA 会直接 403 (error code: 1010)
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
        )
        .json(&body)
        .send()
        .await?;
    let latency_ms = started.elapsed().as_millis() as u64;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "判定端点返回 {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let value: Value = resp.json().await?;
    let answers = value.get("answers").cloned().unwrap_or(Value::Null);
    Ok(Probe {
        latency_ms,
        answers,
    })
}
