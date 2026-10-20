//! AI 阅读助手（P1：只读已缓存正文）
//!
//! 设计要点：
//! - **不做全书预索引**：一本书已缓存正文通常几 MB，提问时按关键词在已缓存章节里召回
//!   Top-K 片段（毫秒级），把片段拼进提示词；模型只允许引用这些片段。
//! - **知识边界**：没读过的章节不可能回答——片段缺失时明确让用户先读/缓存更多章节。
//! - **引用可信**：回答要求按 `[第N章]` 标注，前端用 `title` 反查章节并跳转。
//! - 配置（缺一即视为未启用）：`READER_AI_BASE_URL` / `READER_AI_API_KEY` /
//!   `READER_AI_MODEL`（默认 deepseek-v4.1-flash）/ `READER_AI_MAX_TOKENS`。

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::storage::Storage;

/// 单次问答注入的章节片段总预算（字符）
const CONTEXT_BUDGET: usize = 6000;
/// 每次最多引用几章
const MAX_SNIPPETS: usize = 6;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u64,
}

/// 设置页保存的配置（存 system_settings 键 `ai_settings`），进程内缓存，保存即生效
static RUNTIME: std::sync::OnceLock<std::sync::RwLock<Option<AiConfig>>> =
    std::sync::OnceLock::new();
static LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn runtime() -> &'static std::sync::RwLock<Option<AiConfig>> {
    RUNTIME.get_or_init(|| std::sync::RwLock::new(None))
}

fn write_runtime(cfg: Option<AiConfig>) {
    if let Ok(mut guard) = runtime().write() {
        *guard = cfg;
    }
}

/// 配置来源：saved（设置页保存）/ env（环境变量）/ none
pub fn source() -> &'static str {
    if runtime()
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|_| ()))
        .is_some()
    {
        "saved"
    } else if env_config().is_some() {
        "env"
    } else {
        "none"
    }
}

/// 当前生效配置：设置页保存的优先，其次环境变量
pub fn config() -> Option<AiConfig> {
    if let Ok(guard) = runtime().read() {
        if let Some(cfg) = guard.as_ref() {
            return Some(cfg.clone());
        }
    }
    env_config()
}

/// 首次使用时从数据库载入设置页保存的配置（幂等）
pub async fn ensure_loaded(storage: &Storage) {
    if LOADED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    match storage.get_system_setting("ai_settings").await {
        Ok(Some(raw)) => match serde_json::from_str::<AiConfig>(&raw) {
            Ok(cfg) => {
                tracing::info!("AI 阅读助手: 已载入设置页保存的端点配置");
                write_runtime(Some(cfg));
            }
            Err(e) => tracing::warn!("AI 设置解析失败: {e}"),
        },
        Ok(None) => {}
        Err(e) => tracing::warn!("读取 AI 设置失败: {e}"),
    }
}

/// 保存配置到数据库并立即生效（无需重启）
pub async fn save(storage: &Storage, cfg: AiConfig) -> anyhow::Result<()> {
    let raw = serde_json::to_string(&cfg)?;
    storage.set_system_setting("ai_settings", &raw).await?;
    write_runtime(Some(cfg));
    LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// 清空设置页保存的配置（回退到环境变量；都没有则 AI 面板不可用）
pub async fn clear(storage: &Storage) -> anyhow::Result<()> {
    storage.delete_system_setting("ai_settings").await?;
    write_runtime(None);
    LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// 读取环境变量配置；缺少 base/key 时返回 None（AI 面板整体不可用）
fn env_config() -> Option<AiConfig> {
    let base_url = std::env::var("READER_AI_BASE_URL").ok()?.trim().trim_end_matches('/').to_string();
    let api_key = std::env::var("READER_AI_API_KEY").ok()?.trim().to_string();
    if base_url.is_empty() || api_key.is_empty() {
        return None;
    }
    let model = std::env::var("READER_AI_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "deepseek-v4.1-flash".to_string());
    let max_tokens = std::env::var("READER_AI_MAX_TOKENS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(3500);
    Some(AiConfig { base_url, api_key, model, max_tokens })
}

pub fn enabled() -> bool {
    config().is_some()
}

/// 检索结果：片段 + 已缓存章节数（用于区分"没有缓存"与"没检索到"）
pub struct Retrieval {
    pub snippets: Vec<Snippet>,
    pub cached_chapters: usize,
}

/// 一条检索片段
pub struct Snippet {
    pub index: i64,
    pub title: String,
    pub text: String,
}

/// 常见疑问词/虚词：不参与检索，避免"什么时候/为什么"把整本书都命中
const STOPWORDS: &[&str] = &[
    "什么", "怎么", "为什么", "如何", "哪里", "哪个", "哪些", "多少", "是不是", "有没有", "之后",
    "之前", "后来", "然后", "现在", "这个", "那个", "这些", "那些", "时候", "请问", "帮我", "一下",
    "他们", "我们", "你们", "这里", "那里", "因为", "所以", "但是", "可以", "还是", "就是", "已经",
    "一个", "没有", "知道", "告诉我", "讲讲", "说说", "关于", "相关", "内容", "情节", "哪一章",
];

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 从问题里切关键词：按非中英文数字切分；长汉字串再切 2-gram（最多 24 个）
fn keywords(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in question.chars() {
        if is_cjk(ch) || ch.is_ascii_alphanumeric() {
            cur.push(ch);
        } else {
            push_token(&mut out, &cur);
            cur.clear();
        }
    }
    push_token(&mut out, &cur);
    out
}

fn push_token(out: &mut Vec<String>, raw: &str) {
    let t = raw.trim();
    let n = t.chars().count();
    if n < 2 || t.chars().all(|c| c.is_ascii_digit()) {
        return;
    }
    if STOPWORDS.contains(&t) {
        return;
    }
    if n <= 4 {
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
        return;
    }
    // 长串（整句）切 2-gram 提升召回；过滤停用词与重复
    let chars: Vec<char> = t.chars().collect();
    for i in 0..chars.len().saturating_sub(1) {
        let g: String = chars[i..i + 2].iter().collect();
        if STOPWORDS.contains(&g.as_str()) || out.iter().any(|x| *x == g) {
            continue;
        }
        out.push(g);
        if out.len() >= 24 {
            break;
        }
    }
}

/// 截取一段正文（按字符边界，前后各留一些上下文）
fn window_around(content: &str, needle: &str, limit: usize) -> Option<String> {
    let pos = content.find(needle)?;
    let start = content[..pos].char_indices().rev().nth(250).map(|(i, _)| i).unwrap_or(0);
    let rest: String = content[start..].chars().take(limit).collect();
    Some(rest.trim().to_string())
}

/// 章节标题里的章号（"第12章 xxx" → 12），用于"无命中时取最近章节"
fn title_order(title: &str) -> Option<i64> {
    let digits: String = title
        .chars()
        .skip_while(|c| *c != '第')
        .skip(1)
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<i64>().ok()
}

/// 检索：已缓存章节里按关键词命中数打分，取前几章；每章截 1-2 个命中窗口
pub async fn retrieve(
    storage: &Storage,
    ns: &str,
    book_url: &str,
    question: &str,
    titles: &[String],
    current_title: &str,
) -> Retrieval {
    let chapters = match storage.list_cached_chapters(ns, book_url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("AI 检索读取章节失败: {e}");
            return Retrieval { snippets: Vec::new(), cached_chapters: 0 };
        }
    };
    if chapters.is_empty() {
        return Retrieval { snippets: Vec::new(), cached_chapters: 0 };
    }
    let cached_chapters = chapters.len();
    let kws = keywords(question);
    let mut scored: Vec<(usize, i64, &(i64, String, String))> = Vec::new();
    for ch in &chapters {
        let mut score = 0usize;
        for kw in &kws {
            let hits = ch.2.matches(kw.as_str()).count();
            if hits > 0 {
                score += 1 + hits.min(4);
            }
        }
        if score > 0 {
            scored.push((score, ch.0, ch));
        }
    }
    let mut out: Vec<Snippet> = Vec::new();
    let mut budget = CONTEXT_BUDGET;

    if scored.is_empty() {
        // 没有关键词命中（"主角是谁""刚才发生了什么"这类泛问）：
        // 取开头两章（人物/世界首次出场）+ 当前章及其前一章（最近情节），并说明可能不完全相关
        let order_of = |title: &str| -> i64 {
            if let Some(pos) = titles.iter().position(|t| t == title) {
                return pos as i64;
            }
            title_order(title).unwrap_or(-1)
        };
        let mut ordered: Vec<&(i64, String, String)> = chapters.iter().collect();
        ordered.sort_by_key(|ch| order_of(&ch.1));
        // 开头两章
        let mut picked: Vec<&(i64, String, String)> = ordered.iter().take(2).copied().collect();
        // 当前章 + 前一章
        if !current_title.trim().is_empty() {
            if let Some(pos) = ordered.iter().position(|ch| ch.1 == current_title) {
                if pos > 0 {
                    picked.push(ordered[pos - 1]);
                }
                picked.push(ordered[pos]);
            }
        }
        for ch in ordered.iter().rev().take(1) {
            picked.push(ch);
        }
        // 去重（按缓存键）
        picked.sort_by_key(|ch| ch.0);
        picked.dedup_by_key(|ch| ch.0);
        for ch in picked {
            if budget == 0 {
                break;
            }
            let text: String = ch.2.chars().take(budget.min(1400)).collect();
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            budget = budget.saturating_sub(text.chars().count());
            out.push(Snippet { index: ch.0, title: ch.1.clone(), text: text.to_string() });
        }
        return Retrieval { snippets: out, cached_chapters };
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (_, _, ch) in scored.iter().take(MAX_SNIPPETS) {
        if budget == 0 {
            break;
        }
        let mut parts: Vec<String> = Vec::new();
        for kw in &kws {
            if parts.len() >= 2 {
                break;
            }
            if let Some(w) = window_around(&ch.2, kw, budget.min(900)) {
                if !parts.iter().any(|p| p == &w) {
                    parts.push(w);
                }
            }
        }
        if parts.is_empty() {
            let text: String = ch.2.chars().take(budget.min(900)).collect();
            parts.push(text.trim().to_string());
        }
        let text = parts.join("\n…\n");
        budget = budget.saturating_sub(text.chars().count());
        out.push(Snippet { index: ch.0, title: ch.1.clone(), text });
    }
    Retrieval { snippets: out, cached_chapters }
}

/// 组装对话消息：系统提示（含片段/规则）+ 最近 4 条历史 + 本轮问题
pub fn build_messages(
    book_name: &str,
    question: &str,
    snippets: &[Snippet],
    cached_chapters: usize,
    history: &[(String, String)],
) -> Vec<Value> {
    let system = if snippets.is_empty() && cached_chapters == 0 {
        format!(
            "你是小说阅读助手。当前书籍《{book_name}》还没有可用的已缓存章节正文。\n\
             请直接告诉用户：需要先阅读一些章节（或开启整书缓存）后我才能回答，不要编造任何情节。"
        )
    } else if snippets.is_empty() {
        format!(
            "你是小说阅读助手。当前书籍《{book_name}》已缓存 {cached_chapters} 章正文，\n\
             但这次没有检索到与问题相关的片段。请告诉用户：换个更具体的说法（提到人名/物品名）或先读更多章节；\n\
             不要编造情节。"
        )
    } else {
        let mut ctx = String::new();
        for s in snippets {
            ctx.push_str(&format!("\n【{}】\n{}\n", s.title, s.text));
        }
        format!(
            "你是小说阅读助手，帮读者回忆情节、人物何时出场、物品何时获得、人物关系。\n\
             规则：\n\
             1) 只能依据下面给出的章节片段回答，不得编造片段之外的情节；\n\
             2) 每个结论后用 [章节标题] 标注依据，例如 [第三章 夜访]；\n\
             3) 片段里没有的信息，直接说“已缓存的章节里没有提到”；\n\
             4) 回答简洁，可用短列表；人物关系用 “A → B：关系” 的紧凑写法。\n\n\
             书籍：《{book_name}》\n\n【已缓存章节片段】{ctx}"
        )
    };
    let mut msgs = vec![json!({ "role": "system", "content": system })];
    let start = history.len().saturating_sub(4);
    for (role, content) in &history[start..] {
        msgs.push(json!({ "role": role, "content": content }));
    }
    msgs.push(json!({ "role": "user", "content": question }));
    msgs
}

/// 调上游 OpenAI 兼容端点并把增量转发成 SSE 事件（meta 由调用方先发）
pub async fn stream_chat(
    cfg: AiConfig,
    messages: Vec<Value>,
    tx: UnboundedSender<Result<Bytes, Infallible>>,
) {
    let send = |payload: Value| {
        let _ = tx.send(Ok(Bytes::from(format!("data: {payload}\n\n"))));
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            send(json!({ "type": "error", "message": format!("初始化模型客户端失败: {e}") }));
            return;
        }
    };
    let max_tokens = cfg.max_tokens;
    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "max_tokens": max_tokens,
    });
    let resp = match client
        .post(format!("{}/chat/completions", cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            send(json!({ "type": "error", "message": format!("模型连接失败: {e}") }));
            return;
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        send(json!({
            "type": "error",
            "message": format!("模型返回 {status}: {}", text.chars().take(300).collect::<String>()),
        }));
        return;
    }

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut thinking_chars = 0usize;
    let mut thinking_sent = 0usize;
    let mut saw_content = false;
    let mut finish_reason = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                send(json!({ "type": "error", "message": format!("读取模型响应失败: {e}") }));
                return;
            }
        };
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let delta = &v["choices"][0]["delta"];
            // 推理模型先吐 reasoning_content：只回报进度，不当作答案
            if let Some(r) = delta.get("reasoning_content").and_then(|x| x.as_str()) {
                thinking_chars += r.chars().count();
                if thinking_chars - thinking_sent >= 200 {
                    thinking_sent = thinking_chars;
                    send(json!({ "type": "thinking", "chars": thinking_chars }));
                }
            }
            if let Some(fr) = v["choices"][0]["finish_reason"].as_str() {
                finish_reason = fr.to_string();
            }
            if let Some(c) = delta.get("content").and_then(|x| x.as_str()) {
                if !c.is_empty() {
                    saw_content = true;
                    send(json!({ "type": "delta", "text": c }));
                }
            }
        }
    }
    // 推理模型把输出预算花在思考上时 content 为空: 给出可行动提示, 而不是让面板显示"(无内容)"
    if !saw_content {
        let message = if finish_reason == "length" {
            format!(
                "模型在思考阶段用尽了输出预算(当前 max_tokens={max_tokens})。\n请到「设置 → AI 阅读助手」把最大输出 tokens 调大(建议 4000+), 或换一个非推理模型。"
            )
        } else {
            "模型没有返回内容, 请重试, 或在设置里更换模型。".to_string()
        };
        send(json!({ "type": "error", "message": message }));
    } else {
        send(json!({ "type": "done" }));
    }
}

/// 非流式对话（用于结构化输出：人物关系图 JSON）
pub async fn chat_once(
    cfg: &AiConfig,
    messages: Vec<Value>,
    max_tokens: u64,
) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": false,
        "max_tokens": max_tokens,
    });
    let resp = client
        .post(format!("{}/chat/completions", cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("模型返回 {status}: {}", text.chars().take(200).collect::<String>());
    }
    let v: Value = resp.json().await?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if content.trim().is_empty() {
        anyhow::bail!("模型没有返回内容（推理模型可能在思考阶段用尽了 token 预算）");
    }
    Ok(content)
}

/// 拼人物关系图的系统提示
pub fn build_graph_messages(
    book_name: &str,
    snippets: &[Snippet],
    question: &str,
) -> Vec<Value> {
    let mut ctx = String::new();
    for s in snippets {
        ctx.push_str(&format!("\n【{}】\n{}\n", s.title, s.text));
    }
    let system = format!(
        "你是小说人物关系抽取器。只依据给定章节片段抽取人物与关系，不得编造。\n\
         只输出 JSON（不要解释、不要 markdown 代码块），结构：\n\
         {{\"characters\":[{{\"name\":\"人物名\",\"alias\":\"别名/称号(可空)\",\"firstChapter\":\"片段中出现的章节标题\",\"role\":\"身份或一句话定位\"}}],\n\
         \"relations\":[{{\"from\":\"人物A\",\"to\":\"人物B\",\"label\":\"关系(≤8字)\",\"chapter\":\"依据章节标题\"}}]}}\n\
         约束：characters 取 5-15 个最重要的人物；relations 5-15 条，from/to 必须是 characters 里的 name；\n\
         firstChapter/chapter 必须逐字取自片段里的【标题】；片段没有的信息留空串，不要猜。\n\n\
         书籍：《{book_name}》\n\n【章节片段】{ctx}"
    );
    let user = if question.trim().is_empty() {
        "请抽取主要人物及其关系。".to_string()
    } else {
        format!("重点关注：{question}")
    };
    vec![json!({ "role": "system", "content": system }), json!({ "role": "user", "content": user })]
}

/// 从模型输出中提取 JSON 对象（容忍 ```json 包裹 / 前后解释文字）
pub fn extract_json_object(raw: &str) -> Option<Value> {
    let text = raw.trim();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end]).ok()
}
