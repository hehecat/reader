//! 书籍 API 跨子模块复用的内部辅助（参数解析、配额检查、本地书路径解析与 TXT 目录规则）

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::api::router::{AppState, ReturnData};

fn json_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

/// 整型参数提取：JSON Body 一等公民（兼容数字型与字符串型如 `3` / `"3"`），Query 参数兜底
pub(super) fn int_param_of(
    params: &HashMap<String, String>,
    body: Option<&Value>,
    keys: &[&str],
) -> Option<i64> {
    if let Some(b) = body {
        for k in keys {
            if let Some(v) = b.get(*k).and_then(json_i64) {
                return Some(v);
            }
        }
    }
    for k in keys {
        if let Some(v) = params.get(*k).and_then(|s| s.trim().parse::<i64>().ok()) {
            return Some(v);
        }
    }
    None
}

/// 非空字符串参数提取：JSON Body 一等公民（按候选 key 顺序匹配），Query 参数兜底
pub(super) fn str_param_of(
    params: &HashMap<String, String>,
    body: Option<&Value>,
    keys: &[&str],
) -> String {
    if let Some(b) = body {
        for k in keys {
            if let Some(v) = b.get(*k) {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() {
                        return s.to_string();
                    }
                } else if !v.is_null() {
                    return v.to_string();
                }
            }
        }
    }
    for k in keys {
        if let Some(s) = params.get(*k) {
            if !s.is_empty() {
                return s.clone();
            }
        }
    }
    String::new()
}

/// 可选字符串参数提取（保留空字符串语义）：JSON Body 一等公民，Query 参数兜底
pub(super) fn opt_str_param_of(
    params: &HashMap<String, String>,
    body: Option<&Value>,
    keys: &[&str],
) -> Option<String> {
    if let Some(b) = body {
        for k in keys {
            if let Some(s) = b.get(*k).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    for k in keys {
        if let Some(s) = params.get(*k) {
            return Some(s.clone());
        }
    }
    None
}

pub(super) async fn ensure_book_quota(
    state: &AppState,
    ns: &str,
    book_url: &str,
) -> Result<(), ReturnData> {
    if state
        .storage
        .find_book(ns, book_url)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return Ok(());
    }
    let Some(limit) = state.storage.book_limit_for(ns).await.ok().flatten() else {
        return Ok(());
    };
    if limit <= 0 {
        return Ok(());
    }
    let count = state.storage.count_books_for_user(ns).await.map_err(|e| {
        tracing::error!("统计书籍数失败: {e}");
        ReturnData::err("系统错误")
    })?;
    if count >= limit {
        return Err(ReturnData::err("你已达到书籍数上限，请联系管理员"));
    }
    Ok(())
}

pub(crate) async fn txt_toc_rule_regexes(state: &AppState, ns: &str) -> Vec<String> {
    match state.storage.get_txt_toc_rules(ns).await {
        Ok(rules) => rules
            .into_iter()
            .filter(|r| r.enable && !r.rule.trim().is_empty())
            .map(|r| r.rule)
            .collect(),
        Err(e) => {
            tracing::warn!("getTxtTocRules 失败（回退默认规则）: {e}");
            Vec::new()
        }
    }
}

pub(super) fn resolve_storage_path(storage_dir: &Path, book_url: &str) -> Option<PathBuf> {
    let rel = book_url.trim_start_matches("storage/");
    let candidate = storage_dir.join(rel);
    let abs = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());
    let root = storage_dir
        .canonicalize()
        .unwrap_or_else(|_| storage_dir.to_path_buf());
    if abs.starts_with(&root) && abs.is_file() {
        Some(abs)
    } else {
        None
    }
}

pub(crate) fn resolve_local_file_path(storage_dir: &Path, book_url: &str) -> Option<PathBuf> {
    resolve_storage_path(storage_dir, book_url)
        .or_else(|| resolve_loc_book_file(storage_dir, book_url))
}

pub(crate) fn resolve_loc_book_file(storage_dir: &Path, book_url: &str) -> Option<PathBuf> {
    let path = storage_dir.join(book_url.trim_start_matches("storage/"));
    let mut found: Option<PathBuf> = None;
    if path.is_file() {
        found = Some(path.clone());
    }
    if found.is_none() && path.is_dir() {
        let idx = path.join("index.epub");
        if idx.is_file() {
            found = Some(idx);
        } else {
            let rd = std::fs::read_dir(&path).ok()?;
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() && p.to_string_lossy().to_lowercase().ends_with(".epub") {
                    found = Some(p);
                    break;
                }
            }
        }
    }
    if found.is_none() {
        let parent = path.parent()?;
        let idx = parent.join("index.epub");
        if idx.is_file() {
            found = Some(idx);
        } else {
            let rd = std::fs::read_dir(parent).ok()?;
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    let lower = p.to_string_lossy().to_lowercase();
                    if lower.ends_with(".epub") || lower.ends_with(".txt") {
                        found = Some(p);
                        break;
                    }
                }
            }
        }
    }
    let p = found?;
    let abs = p.canonicalize().unwrap_or_else(|_| p.clone());
    let root = storage_dir
        .canonicalize()
        .unwrap_or_else(|_| storage_dir.to_path_buf());
    if abs.starts_with(&root) && abs.is_file() {
        Some(abs)
    } else {
        None
    }
}
