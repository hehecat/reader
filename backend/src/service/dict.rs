//! 离线词典查询: 英汉(ECDICT) + 汉语(新华字典字词/词语/成语)
//!
//! 数据源(构建期下载到 READER_DICT_DIR, 默认 /app/dict, 原始文件不进仓库):
//! - ecdict.csv      英→汉(词/音标/释义)          skywind3000/ECDICT (MIT)
//! - word.json       汉字与词条(拼音/部首/释义)    pwxcoo/chinese-xinhua (MIT)
//! - ci.json         词语释义                      pwxcoo/chinese-xinhua (MIT)
//! - idiom.json      成语(释义/出处/例句)          pwxcoo/chinese-xinhua (MIT)
//!
//! 首次使用时把原始文件导入 {workdir}/storage/dict.sqlite(持久化), 之后按词精确查询;
//! 导入在启动后台任务里做, 查询遇到未就绪返回 status="building" 由前端轮询。
//!
//! 本地未命中时的在线兜底(READER_DICT_ONLINE=0 关闭): 有道词典 jsonapi(免密钥),
//! 结果写入 online 表缓存 30 天(离线/限流时可继续命中); 网络失败静默降级为「未找到」。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use parking_lot::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    /// en | char | word | idiom | online | baike | web
    pub kind: String,
    pub word: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phonetic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinyin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DictStatus {
    /// 可用
    Ok,
    /// 数据导入中(首次启动)
    Building,
    /// 未安装词典数据(构建时未下载 / READER_DICT_DIR 无文件)
    Unavailable,
}

pub struct LookupResult {
    pub status: DictStatus,
    pub entries: Vec<DictEntry>,
}

struct DictState {
    pool: Option<SqlitePool>,
    building: bool,
    unavailable: bool,
}

static STATE: LazyLock<Mutex<DictState>> = LazyLock::new(|| {
    Mutex::new(DictState {
        pool: None,
        building: false,
        unavailable: false,
    })
});

/// 词典原始数据目录: env READER_DICT_DIR, 默认 /app/dict
pub fn dict_dir() -> PathBuf {
    std::env::var("READER_DICT_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/app/dict"))
}

/// 在线兜底缓存 TTL(30 天): 首次联网查询后离线也能命中
const ONLINE_TTL_MS: i64 = 30 * 24 * 3600 * 1000;

/// 在线解析器版本: 解析逻辑变更时 +1, 旧缓存自动失效(避免旧解析结果压住新字段)
const ONLINE_VER: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OnlineCache {
    ver: u32,
    entries: Vec<DictEntry>,
}

/// 在线兜底开关(默认开; READER_DICT_ONLINE=0 纯离线)
fn online_enabled() -> bool {
    std::env::var("READER_DICT_ONLINE")
        .map(|v| v.trim() != "0" && !v.trim().eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn raw_files(dir: &Path) -> Vec<&'static str> {
    ["ecdict.csv", "word.json", "ci.json", "idiom.json"]
        .into_iter()
        .filter(|name| dir.join(name).is_file())
        .collect()
}

/// 启动时调用: 若词典数据存在且库未建好, 后台导入(不阻塞启动)
pub fn spawn_build_if_needed(storage_dir: &Path) {
    let dir = dict_dir();
    if raw_files(&dir).is_empty() {
        if online_enabled() {
            // 无本地数据但允许在线: 仍建空库(仅 online 缓存表), 查词走在线兜底
            tracing::info!("未安装本地词典数据, 仅在线兜底(有道)可用");
            let db_path = storage_dir.join("dict.sqlite");
            if !db_path.is_file() || !lib_ready(&db_path, &dict_dir()) {
                let dir_owned = dict_dir();
                let db_owned = db_path.clone();
                std::thread::spawn(move || {
                    let _ = build_dict_atomic(&dir_owned, &db_owned);
                });
            }
        } else {
            tracing::info!("词典数据未安装({} 无数据文件), 选中查词不可用", dir.display());
            STATE.lock().unavailable = true;
        }
        return;
    }
    let db_path = storage_dir.join("dict.sqlite");
    if lib_ready(&db_path, &dir) {
        tracing::info!("词典库就绪: {}", db_path.display());
        return;
    }
    tracing::info!("词典库缺失或未完成, 开始导入: {}", db_path.display());
    STATE.lock().building = true;
    let dir = dir.clone();
    std::thread::spawn(move || {
        if let Err(e) = build_dict_atomic(&dir, &db_path) {
            tracing::warn!("词典库导入失败: {e}");
            let mut st = STATE.lock();
            st.building = false;
            st.unavailable = true;
            // 失败: 后续查询走 unavailable 提示, 不再报 building
        }
    });
}

/// 就绪标记路径: 导入完全成功后才写, 用于识别半成品库
fn ready_marker(db_path: &Path) -> std::path::PathBuf {
    db_path.with_extension("sqlite.ready")
}

/// 库是否可用: 存在 + 够大 + 有就绪标记 + 标记不旧于原始数据文件
fn lib_ready(db_path: &Path, dir: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(db_path) else {
        return false;
    };
    if meta.len() < 1024 * 1024 {
        return false;
    }
    let Ok(marker) = std::fs::metadata(ready_marker(db_path)) else {
        return false;
    };
    let Ok(marker_time) = marker.modified() else {
        return false;
    };
    // 原始数据比标记新 → 需要重建
    for f in raw_files(dir) {
        if let Ok(m) = std::fs::metadata(&f) {
            if m.modified().map(|t| t > marker_time).unwrap_or(false) {
                return false;
            }
        }
    }
    true
}

/// 构建到临时文件后原子替换, 成功才写就绪标记(避免半成品被复用)
fn build_dict_atomic(dir: &Path, db_path: &Path) -> Result<()> {
    let tmp = db_path.with_extension("sqlite.tmp");
    let _ = std::fs::remove_file(&tmp);
    build_dict(dir, &tmp)?;
    std::fs::rename(&tmp, db_path)?;
    let _ = std::fs::remove_file(ready_marker(db_path));
    std::fs::write(ready_marker(db_path), b"ok")?;
    Ok(())
}

/// 导入原始文件 → dict.sqlite (幂等: 已存在表则跳过)
fn build_dict(dir: &Path, db_path: &Path) -> Result<()> {
    let t0 = std::time::Instant::now();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        if db_path.exists() {
            let _ = std::fs::remove_file(db_path);
        }
        let opts = SqliteConnectOptions::new().filename(db_path).create_if_missing(true);
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS en(word TEXT PRIMARY KEY, phonetic TEXT, translation TEXT, definition TEXT)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS zh(word TEXT PRIMARY KEY, kind TEXT, pinyin TEXT, explanation TEXT)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS online(word TEXT PRIMARY KEY, payload TEXT, updated_at INTEGER)",
        )
        .execute(&pool)
        .await?;

        // ---- ECDICT 英汉 ----
        let en_path = dir.join("ecdict.csv");
        if en_path.is_file() {
            let file = std::fs::File::open(&en_path)?;
            let mut reader = BufReader::with_capacity(1 << 20, file);
            let mut line = String::new();
            let mut tx = pool.begin().await?;
            let mut n = 0usize;
            let mut header = true;
            while reader.read_line(&mut line)? > 0 {
                if header {
                    header = false;
                    line.clear();
                    continue;
                }
                let fields = split_csv_line(line.trim_end_matches(['\n', '\r']));
                if fields.len() >= 4 {
                    let word = fields[0].trim().to_lowercase();
                    if !word.is_empty() {
                        let translation = fields[3].replace("\\n", "\n");
                        sqlx::query(
                            "INSERT OR REPLACE INTO en(word, phonetic, translation, definition) VALUES (?1, ?2, ?3, ?4)",
                        )
                        .bind(&word)
                        .bind(fields[1].trim())
                        .bind(&translation)
                        .bind(&fields[2])
                        .execute(&mut *tx)
                        .await?;
                        n += 1;
                        if n % 20000 == 0 {
                            tx.commit().await?;
                            tx = pool.begin().await?;
                        }
                    }
                }
                line.clear();
            }
            tx.commit().await?;
            tracing::info!("词典: ECDICT 导入 {n} 条");
        }

        // ---- 新华字典: 字词 / 词语 / 成语 ----
        let zh_files: [(&str, &'static str); 3] = [
            ("word.json", "word"),
            ("ci.json", "word"),
            ("idiom.json", "idiom"),
        ];
        for (name, kind) in zh_files {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let items: Vec<serde_json::Value> = serde_json::from_str(&text)?;
            let mut tx = pool.begin().await?;
            let mut n = 0usize;
            for item in items {
                let word = item
                    .get("word")
                    .or_else(|| item.get("ci"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if word.is_empty() {
                    continue;
                }
                let pinyin = item.get("pinyin").and_then(|v| v.as_str()).map(str::to_string);
                let body = item
                    .get("explanation")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_default();
                if body.is_empty() {
                    continue;
                }
                // 同词多来源: 保留首条 (word.json 在先)
                sqlx::query(
                    "INSERT OR IGNORE INTO zh(word, kind, pinyin, explanation) VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(&word)
                .bind(kind)
                .bind(&pinyin)
                .bind(&body)
                .execute(&mut *tx)
                .await?;
                n += 1;
                if n % 20000 == 0 {
                    tx.commit().await?;
                    tx = pool.begin().await?;
                }
            }
            tx.commit().await?;
            tracing::info!("词典: {name} 导入 {n} 条");
        }

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_zh_word ON zh(word)").execute(&pool).await?;
        pool.close().await;
        Ok::<(), anyhow::Error>(())
    })?;
    tracing::info!("词典库构建完成({}ms): {}", t0.elapsed().as_millis(), db_path.display());
    // 复位 building: 之后查询走惰性开库路径(否则会一直返回 building)
    STATE.lock().building = false;
    Ok(())
}

/// CSV 单行拆列(支持双引号包裹与转义双引号; ECDICT 字段内逗号/换行以字面 \n 表示)
fn split_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(8);
    let mut buf = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if quoted && chars.peek() == Some(&'"') {
                    buf.push('"');
                    chars.next();
                } else {
                    quoted = !quoted;
                }
            }
            ',' if !quoted => {
                out.push(std::mem::take(&mut buf));
            }
            _ => buf.push(c),
        }
    }
    out.push(buf);
    out
}

fn is_english(word: &str) -> bool {
    word.chars().all(|c| c.is_ascii_alphabetic() || c == ' ' || c == '-' || c == '\'')
}

/// 查询: 英文精确(小写) → 中文精确 → 中文前缀(≤6 字)
pub async fn lookup(storage_dir: &Path, word: &str) -> Result<LookupResult> {
    let query = word.trim().trim_matches(|c: char| {
        !c.is_alphanumeric() && !('\u{4e00}'..='\u{9fff}').contains(&c)
    });
    if query.is_empty() {
        return Ok(LookupResult { status: DictStatus::Ok, entries: Vec::new() });
    }
    {
        let st = STATE.lock();
        if st.unavailable {
            return Ok(LookupResult { status: DictStatus::Unavailable, entries: Vec::new() });
        }
        if st.building && st.pool.is_none() {
            return Ok(LookupResult { status: DictStatus::Building, entries: Vec::new() });
        }
    }
    // 惰性开库(构建完成/库已存在的场景); 连接在锁外 await
    let db_path = storage_dir.join("dict.sqlite");
    let existing = STATE.lock().pool.clone();
    let pool = if let Some(p) = existing {
        Some(p)
    } else if db_path.is_file() {
        let opts = SqliteConnectOptions::new().filename(&db_path);
        let pool = SqlitePoolOptions::new().max_connections(2).connect_with(opts).await?;
        {
            let mut st = STATE.lock();
            st.pool = Some(pool.clone());
            st.building = false;
        }
        Some(pool)
    } else {
        None
    };
    let Some(pool) = pool else {
        let building = STATE.lock().building;
        return Ok(LookupResult {
            status: if building { DictStatus::Building } else { DictStatus::Unavailable },
            entries: Vec::new(),
        });
    };

    let mut entries = Vec::new();
    if is_english(query) {
        let lower = query.to_lowercase();
        let rows: Vec<(String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT word, phonetic, translation, definition FROM en WHERE word = ?1",
        )
        .bind(&lower)
        .fetch_all(&pool)
        .await?;
        for (w, phonetic, translation, definition) in rows {
            let body = translation.clone().filter(|t| !t.trim().is_empty()).or(definition);
            entries.push(DictEntry {
                kind: "en".to_string(),
                word: w,
                phonetic: phonetic.filter(|p| !p.trim().is_empty()),
                pinyin: None,
                body: body.filter(|b| !b.trim().is_empty()),
                source: "ECDICT".to_string(),
            });
        }
    }
    // 英文词形回退: running→run / boxes→box / played→play
    if entries.is_empty() && is_english(query) {
        for cand in lemma_candidates(&query.to_lowercase()) {
            let rows: Vec<(String, Option<String>, Option<String>, Option<String>)> =
                sqlx::query_as("SELECT word, phonetic, translation, definition FROM en WHERE word = ?1")
                    .bind(&cand)
                    .fetch_all(&pool)
                    .await?;
            if !rows.is_empty() {
                for (w, phonetic, translation, definition) in rows {
                    let body = translation.filter(|t| !t.trim().is_empty()).or(definition);
                    entries.push(DictEntry {
                        kind: "en".to_string(),
                        word: w,
                        phonetic: phonetic.filter(|p| !p.trim().is_empty()),
                        pinyin: None,
                        body: body.filter(|b| !b.trim().is_empty()),
                        source: "ECDICT(词形回退)".to_string(),
                    });
                }
                break;
            }
        }
    }
    if entries.is_empty() {
        let rows: Vec<(String, String, Option<String>, String)> =
            sqlx::query_as("SELECT word, kind, pinyin, explanation FROM zh WHERE word = ?1")
                .bind(query)
                .fetch_all(&pool)
                .await?;
        for (w, kind, pinyin, explanation) in rows {
            entries.push(DictEntry {
                kind: if kind == "idiom" {
                    "idiom".to_string()
                } else if w.chars().count() == 1 {
                    "char".to_string()
                } else {
                    "word".to_string()
                },
                word: w,
                phonetic: None,
                pinyin: pinyin.filter(|p| !p.trim().is_empty()),
                body: Some(explanation),
                source: "新华字典".to_string(),
            });
        }
    }
    // 在线兜底: 本地未命中 → 有道(jsonapi 免密钥), 结果缓存 30 天
    if entries.is_empty() && online_enabled() {
        entries = lookup_online(&pool, query).await;
    }
    // 前缀兜底(最后手段): 中文长选段未命中时取最长命中前缀, 避免误当前缀词的释义压过在线结果
    if entries.is_empty() && !is_english(query) {
        let chars: Vec<char> = query.chars().collect();
        for len in (2..=chars.len().min(6)).rev() {
            let prefix: String = chars[..len].iter().collect();
            let rows: Vec<(String, String, Option<String>, String)> =
                sqlx::query_as("SELECT word, kind, pinyin, explanation FROM zh WHERE word = ?1")
                    .bind(&prefix)
                    .fetch_all(&pool)
                    .await?;
            if !rows.is_empty() {
                for (w, kind, pinyin, explanation) in rows {
                    entries.push(DictEntry {
                        kind: if kind == "idiom" {
                            "idiom".to_string()
                        } else if w.chars().count() == 1 {
                            "char".to_string()
                        } else {
                            "word".to_string()
                        },
                        word: w,
                        phonetic: None,
                        pinyin: pinyin.filter(|p| !p.trim().is_empty()),
                        body: Some(explanation),
                        source: "新华字典(前缀)".to_string(),
                    });
                }
                break;
            }
        }
    }
    Ok(LookupResult { status: DictStatus::Ok, entries })
}

/// 英文词形剥离候选(粗粒度, 覆盖常见屈折)
fn lemma_candidates(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    let w = word.trim();
    if w.len() < 4 {
        return out;
    }
    if let Some(base) = w.strip_suffix("ies") {
        out.push(format!("{base}y"));
    }
    if let Some(base) = w.strip_suffix("es") {
        out.push(base.to_string());
    }
    if let Some(base) = w.strip_suffix('s') {
        out.push(base.to_string());
    }
    if let Some(base) = w.strip_suffix("ing") {
        out.push(base.to_string());
        out.push(format!("{base}e"));
        if base.len() > 2 {
            let mut doubled = base.to_string();
            let last = doubled.pop().unwrap();
            if doubled.ends_with(last) {
                out.push(doubled);
            } else {
                out.push(format!("{base}{last}"));
            }
        }
    }
    if let Some(base) = w.strip_suffix("ed") {
        out.push(base.to_string());
        out.push(format!("{base}e"));
    }
    out.retain(|c| c.len() >= 2 && c != w);
    out.dedup();
    out
}

/// 混合节点取文本: 字符串 或 {"#text": "..."} (有道 jsonapi 的 l.i 混排)
fn node_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(o) => o
            .get("#text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// 提取 [{"tr":[{"l":{"i":[...]}}]}] 形态的释义行(兼容 tr 为对象的老形态)
fn trs_lines(word_obj: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(trs) = word_obj.get("trs").and_then(|v| v.as_array()) else {
        return out;
    };
    for item in trs {
        let Some(tr) = item.get("tr") else {
            continue;
        };
        let list: Vec<&serde_json::Value> = match tr {
            serde_json::Value::Array(arr) => arr.iter().collect(),
            other => vec![other],
        };
        for t in list {
            let Some(items) = t.get("l").and_then(|l| l.get("i")) else {
                continue;
            };
            let line: String = items
                .as_array()
                .map(|arr| arr.iter().map(node_text).collect::<Vec<_>>().join(""))
                .unwrap_or_default();
            let line = line.trim().to_string();
            if !line.is_empty() {
                out.push(line);
            }
        }
    }
    out
}

/// 在线查询(有道 jsonapi): ec/ce 释义 + baike 百科 + web_trans 网络释义; 结果缓存 30 天
async fn lookup_online(pool: &SqlitePool, word: &str) -> Vec<DictEntry> {
    let now = now_millis();
    // 缓存命中(30 天内)
    let cached: Option<(String, i64)> =
        sqlx::query_as("SELECT payload, updated_at FROM online WHERE word = ?1")
            .bind(word)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if let Some((payload, updated)) = cached {
        if now - updated <= ONLINE_TTL_MS {
            if let Ok(blob) = serde_json::from_str::<OnlineCache>(payload.as_str()) {
                if blob.ver == ONLINE_VER {
                    return blob.entries;
                }
            }
        }
    }
    let url = format!("https://dict.youdao.com/jsonapi?q={}", urlencode(word));
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("Mozilla/5.0 (ReaderDict)")
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // net_ok=false(有道或百科源请求失败)时本次不写缓存, 避免把"暂无介绍"固化 30 天
    let mut net_ok = true;
    let json = match client.get(&url).send().await {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(v) => Some(v),
            Err(_) => {
                net_ok = false;
                None
            }
        },
        Err(_) => {
            net_ok = false;
            None
        }
    };
    let mut entries = Vec::new();
    let mut yd_baike: Option<DictEntry> = None;
    let mut yd_wiki: Option<DictEntry> = None;
    if let Some(json) = json {
        // 英汉 / 中英 释义
        for key in ["ec", "ce"] {
            let Some(words) = json.get(key).and_then(|v| v.get("word")).and_then(|v| v.as_array()) else {
                continue;
            };
            let mut lines = Vec::new();
            let mut phonetic = None;
            for w in words {
                phonetic = phonetic.or_else(|| {
                    w.get("usphone")
                        .or_else(|| w.get("ukphone"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .filter(|s| !s.trim().is_empty())
                });
                lines.extend(trs_lines(w));
            }
            if !lines.is_empty() {
                entries.push(DictEntry {
                    kind: "online".to_string(),
                    word: word.to_string(),
                    phonetic,
                    pinyin: simple_pinyin(&json),
                    body: Some(lines.join("
")),
                    source: "有道词典".to_string(),
                });
            }
        }
        if let Some(e) = newhh_entry(&json, word) {
            entries.push(e);
        }
        // 百科摘要(有道; 中文查询时仅作兜底, 优先百度百科)
        if let Some(summary) = json
            .get("baike")
            .and_then(|v| v.get("summarys"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|s| s.get("summary"))
            .and_then(|v| v.as_str())
        {
            let summary = summary.trim();
            if !summary.is_empty() {
                let src = json
                    .get("baike")
                    .and_then(|v| v.get("source"))
                    .and_then(|s| s.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("百科");
                yd_baike = Some(DictEntry {
                    kind: "baike".to_string(),
                    word: word.to_string(),
                    phonetic: None,
                    pinyin: None,
                    body: Some(summary.to_string()),
                    source: if src.contains("百度") { "百度百科".to_string() } else { "百科".to_string() },
                });
            }
        }
        // 网络释义(取前 3 条)
        if let Some(trans) = json
            .get("web_trans")
            .and_then(|v| v.get("web-translation"))
            .and_then(|v| v.as_array())
        {
            let mut lines = Vec::new();
            for item in trans.iter().take(3) {
                let Some(vals) = item.get("trans").and_then(|v| v.as_array()) else {
                    continue;
                };
                let joined = vals
                    .iter()
                    .filter_map(|t| t.get("value").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("；");
                let key = item.get("key").and_then(|v| v.as_str()).unwrap_or("");
                if !joined.is_empty() {
                    lines.push(if key.is_empty() { joined } else { format!("{key}: {joined}") });
                }
            }
            if !lines.is_empty() {
                entries.push(DictEntry {
                    kind: "web".to_string(),
                    word: word.to_string(),
                    phonetic: None,
                    pinyin: None,
                    body: Some(lines.join("
")),
                    source: "有道网络释义".to_string(),
                });
            }
        }
        yd_wiki = wiki_entry(&json, word);
        if let Some(e) = sents_entry(&json, word) {
            entries.push(e);
        }
    }
    // 百科介绍: 中文查询优先百度百科(有道摘要常为多义词提示), 次选有道维基摘要/中文维基
    if has_cjk(word) {
        let mut chosen = match baike_baidu(&client, word).await {
            Ok(v) => v,
            Err(_) => {
                net_ok = false;
                None
            }
        };
        if chosen.is_none() {
            chosen = yd_wiki.take();
        }
        if chosen.is_none() {
            chosen = match wiki_zh(&client, word).await {
                Ok(v) => v,
                Err(_) => {
                    net_ok = false;
                    None
                }
            };
        }
        if let Some(e) = chosen {
            // 短释义在前, 百科长文紧随(便于人物/专名先看到介绍)
            let pos = if entries.is_empty() { 0 } else { 1 };
            entries.insert(pos, e);
        }
    } else {
        if let Some(e) = yd_baike {
            entries.push(e);
        }
        if let Some(e) = yd_wiki {
            entries.push(e);
        }
    }
    if !entries.is_empty() && net_ok {
        let blob = OnlineCache { ver: ONLINE_VER, entries: entries.clone() };
        if let Ok(payload) = serde_json::to_string(&blob) {
            let _ = sqlx::query(
                "INSERT OR REPLACE INTO online(word, payload, updated_at) VALUES (?1, ?2, ?3)",
            )
            .bind(word)
            .bind(payload)
            .bind(now)
            .execute(pool)
            .await;
        }
    }
    entries
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 拼音: simple.word[0].phone (中文) / usphone|ukphone (英文)
fn simple_pinyin(json: &serde_json::Value) -> Option<String> {
    json.get("simple")
        .and_then(|v| v.get("word"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|w| {
            w.get("phone")
                .or_else(|| w.get("usphone"))
                .or_else(|| w.get("ukphone"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// 《现代汉语规范词典》释义(newhh): 专名/外来词的权威中文解释
fn newhh_entry(json: &serde_json::Value, word: &str) -> Option<DictEntry> {
    let list = json.get("newhh")?.get("dataList")?.as_array()?;
    let mut lines = Vec::new();
    let mut pinyin = None;
    for item in list {
        pinyin = pinyin.or_else(|| {
            item.get("pinyin")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
        });
        let Some(senses) = item.get("sense").and_then(|v| v.as_array()) else {
            continue;
        };
        for sense in senses {
            let cat = sense.get("cat").and_then(|v| v.as_str()).unwrap_or("");
            let defs: Vec<String> = sense
                .get("def")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|d| d.as_str()).map(str::to_string).collect())
                .unwrap_or_default();
            for d in defs {
                let d = d.trim();
                if d.is_empty() {
                    continue;
                }
                lines.push(if cat.is_empty() { d.to_string() } else { format!("【{cat}】{d}") });
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    let src = json
        .get("newhh")
        .and_then(|v| v.get("source"))
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("现代汉语规范词典");
    let src = src.trim_matches(|c| c == '《' || c == '》').to_string();
    Some(DictEntry {
        kind: "newhh".to_string(),
        word: word.to_string(),
        phonetic: None,
        pinyin,
        body: Some(lines.join(
            "
",
        )),
        source: src,
    })
}

/// 维基百科摘要(wikipedia_digest)
fn wiki_entry(json: &serde_json::Value, word: &str) -> Option<DictEntry> {
    let summary = json
        .get("wikipedia_digest")?
        .get("summarys")?
        .as_array()?
        .first()?
        .get("summary")?
        .as_str()?
        .trim()
        .to_string();
    if summary.is_empty() {
        return None;
    }
    Some(DictEntry {
        kind: "wiki".to_string(),
        word: word.to_string(),
        phonetic: None,
        pinyin: None,
        body: Some(summary),
        source: "维基百科".to_string(),
    })
}

/// 双语例句(blng_sents_part, 取前 2 条)
fn sents_entry(json: &serde_json::Value, word: &str) -> Option<DictEntry> {
    let pairs = json.get("blng_sents_part")?.get("sentence-pair")?.as_array()?;
    let mut lines = Vec::new();
    for p in pairs.iter().take(2) {
        let zh = p.get("sentence").and_then(|v| v.as_str()).unwrap_or("").trim();
        if zh.is_empty() {
            continue;
        }
        let en = p
            .get("sentence-translation")
            .or_else(|| p.get("sentence-eng"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        lines.push(if en.is_empty() {
            zh.to_string()
        } else {
            format!("{zh}\n{en}")
        });
    }
    if lines.is_empty() {
        return None;
    }
    Some(DictEntry {
        kind: "sents".to_string(),
        word: word.to_string(),
        phonetic: None,
        pinyin: None,
        body: Some(lines.join(
            "
",
        )),
        source: "例句".to_string(),
    })
}

/// 是否含中日韩汉字(用于决定是否查中文百科)
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 按字符截断(超长加省略号)
fn clamp_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

/// 百度百科词条摘要(openapi 免密钥): 人物/作品/专名介绍最全
async fn baike_baidu(client: &reqwest::Client, word: &str) -> Result<Option<DictEntry>> {
    let url = format!(
        "https://baike.baidu.com/api/openapi/BaikeLemmaCardApi?scope=103&format=json&appid=379020&bk_length=800&bk_key={}",
        urlencode(word)
    );
    let json: serde_json::Value = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await?
        .json()
        .await?;
    let text = json
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            json.get("desc")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
    Ok(text.map(|t| DictEntry {
        kind: "baike".to_string(),
        word: word.to_string(),
        phonetic: None,
        pinyin: None,
        body: Some(clamp_chars(t, 800)),
        source: "百度百科".to_string(),
    }))
}

/// 中文维基百科摘要(extract, 简体): 百度百科无词条时的次选
async fn wiki_zh(client: &reqwest::Client, word: &str) -> Result<Option<DictEntry>> {
    let url = format!(
        "https://zh.wikipedia.org/w/api.php?action=query&prop=extracts&exintro=1&explaintext=1&redirects=1&variant=zh-cn&format=json&titles={}",
        urlencode(word)
    );
    let json: serde_json::Value = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await?
        .json()
        .await?;
    let extract = json
        .get("query")
        .and_then(|q| q.get("pages"))
        .and_then(|p| p.as_object())
        .and_then(|o| o.values().next())
        .and_then(|pg| pg.get("extract"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    Ok(extract.map(|t| DictEntry {
        kind: "wiki".to_string(),
        word: word.to_string(),
        phonetic: None,
        pinyin: None,
        body: Some(clamp_chars(t, 800)),
        source: "维基百科".to_string(),
    }))
}

/// 极简 URL 编码(仅词典查询词片, 避免引入额外依赖)
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
