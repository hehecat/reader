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

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use parking_lot::Mutex;

use anyhow::Result;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize)]
pub struct DictEntry {
    /// en | char | word | idiom
    pub kind: &'static str,
    pub word: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phonetic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinyin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
        tracing::info!("词典数据未安装({} 无数据文件), 选中查词不可用", dir.display());
        STATE.lock().unavailable = true;
        return;
    }
    let db_path = storage_dir.join("dict.sqlite");
    let ready = std::fs::metadata(&db_path).map(|m| m.len() > 1024 * 1024).unwrap_or(false);
    if ready {
        tracing::info!("词典库就绪: {}", db_path.display());
        return;
    }
    STATE.lock().building = true;
    let dir = dir.clone();
    std::thread::spawn(move || {
        if let Err(e) = build_dict(&dir, &db_path) {
            tracing::warn!("词典库导入失败: {e}");
            let mut st = STATE.lock();
            st.building = false;
            st.unavailable = true;
        }
    });
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
    let _ = std::fs::metadata(db_path);
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
                kind: "en",
                word: w,
                phonetic: phonetic.filter(|p| !p.trim().is_empty()),
                pinyin: None,
                body: body.filter(|b| !b.trim().is_empty()),
                source: "ECDICT",
            });
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
                kind: if kind == "idiom" { "idiom" } else if w.chars().count() == 1 { "char" } else { "word" },
                word: w,
                phonetic: None,
                pinyin: pinyin.filter(|p| !p.trim().is_empty()),
                body: Some(explanation),
                source: "新华字典",
            });
        }
        // 前缀兜底: 中文选段(未整词命中)取最长命中前缀
        if entries.is_empty() {
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
                            kind: if kind == "idiom" { "idiom" } else { "word" },
                            word: w,
                            phonetic: None,
                            pinyin: pinyin.filter(|p| !p.trim().is_empty()),
                            body: Some(explanation),
                            source: "新华字典",
                        });
                    }
                    break;
                }
            }
        }
    }
    Ok(LookupResult { status: DictStatus::Ok, entries })
}
