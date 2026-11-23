//! 本地书（TXT / EPUB）导入、预览、文件迁移与目录/正文读取辅助（异步磁盘 I/O + spawn_blocking 解析下沉）

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::extract::{Multipart, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::helpers::{
    ensure_book_quota, resolve_loc_book_file, resolve_storage_path, txt_toc_rule_regexes,
};
use crate::api::auth::resolve_namespace;
use crate::api::router::{
    check_upload_content_length, read_multipart_field_limited, AppState, ReturnData,
};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LocalChapterItem {
    pub title: String,
    pub url: String,
    pub is_volume: bool,
    pub index: i64,
    pub chapter_word_count: i64,
}

#[derive(serde::Serialize)]
pub(super) struct ContentPayload {
    pub content: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewBookMeta {
    name: String,
    author: String,
    kind: String,
    book_url: String,
    origin: &'static str,
    toc_url: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewChapterItem {
    title: String,
    url: String,
    index: usize,
    is_volume: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportPreviewPayload {
    name: String,
    author: String,
    format: String,
    chapter_count: usize,
    preview: Vec<String>,
    book: PreviewBookMeta,
    chapters: Vec<PreviewChapterItem>,
}

/// POST `/reader3/importBookPreview`：上传预览（TXT / EPUB，不入库）
pub async fn import_book_preview(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let max_bytes = state.storage.config.upload_max_bytes();
    let max_mb = state.storage.config.upload_max_mb;
    if let Some(msg) = check_upload_content_length(&headers, max_bytes, max_mb) {
        return Json(ReturnData::err(msg)).into_response();
    }
    let mut file_name = String::new();
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        match multipart.next_field().await {
            Ok(Some(mut field)) => {
                if field.name() == Some("file") {
                    file_name = field.file_name().unwrap_or("file").to_string();
                    match read_multipart_field_limited(&mut field, max_bytes, max_mb).await {
                        Ok(b) => bytes = b,
                        Err(msg) => return Json(ReturnData::err(msg)).into_response(),
                    }
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("importBookPreview multipart 读取失败: {e}");
                break;
            }
        }
    }
    if bytes.is_empty() {
        return Json(ReturnData::err("请上传文件")).into_response();
    }
    let safe_name = Path::new(&file_name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = crate::service::local_book::file_ext(&safe_name);
    if ext.is_empty()
        || !crate::service::local_book::SUPPORTED_EXTENSIONS
            .iter()
            .any(|e| *e == ext)
    {
        return Json(ReturnData::err(format!("不支持导入{ext}格式的书籍文件"))).into_response();
    }
    let user_rules = txt_toc_rule_regexes(&state, &namespace).await;
    match tokio::task::spawn_blocking(move || {
        import_preview_from_bytes(&bytes, &safe_name, &ext, &user_rules)
    })
    .await
    {
        Ok(Ok(payload)) => ReturnData::ok_typed(payload).into_response(),
        Ok(Err(e)) => Json(ReturnData::err(format!("解析失败：{e}"))).into_response(),
        Err(e) => Json(ReturnData::err(format!("解析失败：{e}"))).into_response(),
    }
}

fn import_preview_from_bytes(
    bytes: &[u8],
    file_name: &str,
    ext: &str,
    user_rules: &[String],
) -> anyhow::Result<ImportPreviewPayload> {
    let imported = crate::service::local_book::parse_file_bytes(
        bytes,
        ext,
        user_rules,
        crate::service::local_book::DEFAULT_EPUB_TOC_MODE,
        false,
    )?;
    let (name, author) = local_book_display_meta(file_name, ext, &imported);
    let preview: Vec<String> = imported
        .chapters
        .iter()
        .take(10)
        .map(|c| c.title.clone())
        .collect();
    let book = PreviewBookMeta {
        name: name.clone(),
        author: author.clone(),
        kind: format!("{}{}", imported.format.to_uppercase(), "书籍"),
        book_url: format!("assets/{file_name}"),
        origin: "loc_book",
        toc_url: "",
    };
    let chapters: Vec<PreviewChapterItem> = imported
        .chapters
        .iter()
        .enumerate()
        .map(|(i, c)| PreviewChapterItem {
            title: c.title.clone(),
            url: format!("{file_name}#{i}"),
            index: i,
            is_volume: false,
        })
        .collect();
    Ok(ImportPreviewPayload {
        name,
        author,
        format: imported.format,
        chapter_count: imported.chapters.len(),
        preview,
        book,
        chapters,
    })
}

/// POST `/reader3/uploadLocalBook`：本地书上传入架（仅支持 TXT / EPUB）
pub async fn upload_local_book(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let max_bytes = state.storage.config.upload_max_bytes();
    let max_mb = state.storage.config.upload_max_mb;
    if let Some(msg) = check_upload_content_length(&headers, max_bytes, max_mb) {
        return Json(ReturnData::err(msg)).into_response();
    }
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name = String::new();
    loop {
        match multipart.next_field().await {
            Ok(Some(mut field)) => {
                if field.name() == Some("file") {
                    file_name = field.file_name().unwrap_or("book").to_string();
                    match read_multipart_field_limited(&mut field, max_bytes, max_mb).await {
                        Ok(b) => file_bytes = Some(b),
                        Err(msg) => return Json(ReturnData::err(msg)).into_response(),
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("uploadLocalBook multipart 读取失败: {e}");
                break;
            }
        }
    }
    let Some(bytes) = file_bytes else {
        return Json(ReturnData::err("未收到文件")).into_response();
    };
    if bytes.is_empty() {
        return Json(ReturnData::err("文件为空")).into_response();
    }

    let ext = crate::service::local_book::file_ext(&file_name);
    if !crate::service::local_book::SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
        return Json(ReturnData::err("仅支持 EPUB / TXT 格式")).into_response();
    }
    let user_rules = txt_toc_rule_regexes(&state, &namespace).await;
    let ext_for_parse = ext.clone();
    let (bytes, parsed) = match tokio::task::spawn_blocking(move || {
        let res = if ext_for_parse == "txt" {
            Ok(
                crate::service::local_book::parse_txt_with_rules(&bytes, &user_rules)
                    .unwrap_or_else(|e| {
                        tracing::error!("TXT 解析失败: {e}");
                        crate::service::local_book::ImportedBook {
                            meta: Default::default(),
                            chapters: vec![],
                            cover: None,
                            format: "txt".into(),
                        }
                    }),
            )
        } else {
            crate::service::local_book::parse_file_bytes(
                &bytes,
                &ext_for_parse,
                &user_rules,
                crate::service::local_book::DEFAULT_EPUB_TOC_MODE,
                false,
            )
        };
        (bytes, res)
    })
    .await
    {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!("uploadLocalBook 解析任务异常: {e}");
            return Json(ReturnData::err("解析失败")).into_response();
        }
    };
    let imported = match parsed {
        Ok(b) => b,
        Err(e) => {
            return Json(ReturnData::err(format!(
                "{} 解析失败：{e}",
                ext.to_uppercase()
            )))
            .into_response()
        }
    };

    if imported.chapters.is_empty() {
        return Json(ReturnData::err("未解析到章节内容")).into_response();
    }

    let book_url = format!("local://{}", uuid::Uuid::new_v4());
    let (book_name, book_author) = local_book_display_meta(&file_name, &ext, &imported);
    let book = crate::model::book_chapter::BookInfo {
        name: book_name,
        author: book_author,
        kind: imported.meta.subjects.first().cloned(),
        intro: imported.meta.description.clone(),
        language: imported.meta.language.clone(),
        publisher: imported.meta.publisher.clone(),
        published_at: imported.meta.published_at.clone(),
        toc_url: Some(format!("{book_url}/toc")),
        book_url: book_url.clone(),
        origin: "local".to_string(),
        origin_name: "本地书".to_string(),
        book_type: crate::service::local_book::local_book_type(&ext),
        ..Default::default()
    };

    if let Err(ret) = ensure_book_quota(&state, &namespace, &book_url).await {
        return Json(ret).into_response();
    }
    if let Err(e) = state
        .storage
        .save_local_book(&namespace, &book, &imported)
        .await
    {
        tracing::error!("本地书入库失败: {e}");
        return Json(ReturnData::err("入库失败")).into_response();
    }

    let raw_dir = state
        .storage
        .config
        .storage_dir()
        .join("data")
        .join(&namespace)
        .join("local_files");
    if tokio::fs::create_dir_all(&raw_dir).await.is_ok() {
        let file_id = book.book_url.trim_start_matches("local://");
        let _ = tokio::fs::write(raw_dir.join(format!("{file_id}.{ext}")), &bytes).await;
    }

    if let Some(cover) = &imported.cover {
        let cover_dir = state
            .storage
            .config
            .storage_dir()
            .join("assets")
            .join(&namespace)
            .join("covers");
        let _ = tokio::fs::create_dir_all(&cover_dir).await;
        let file_id = format!("{}.jpg", uuid::Uuid::new_v4());
        if tokio::fs::write(cover_dir.join(&file_id), cover)
            .await
            .is_ok()
        {
            let _ = state
                .storage
                .update_book_cover(
                    &namespace,
                    &book_url,
                    &format!("/assets/{namespace}/covers/{file_id}"),
                )
                .await;
        }
    }

    tracing::info!(
        "本地书导入 [{namespace}]：{}（{} 章）",
        book.name,
        imported.chapters.len()
    );
    ReturnData::ok_typed(book).into_response()
}

// ==================== 本地书迁移、目录与正文读取辅助 ====================

pub(super) async fn migrate_local_book_file(
    storage_dir: &Path,
    namespace: &str,
    name: &str,
    author: &str,
    book_url: &str,
) -> Option<String> {
    let is_temp = book_url.starts_with("/assets/")
        || book_url.starts_with("assets/")
        || book_url.contains("localStore")
        || book_url.contains("webdav");
    if !is_temp {
        return None;
    }
    let trimmed = book_url.trim_start_matches('/');
    let rel = trimmed.strip_prefix("storage/").unwrap_or(trimmed);
    if rel.is_empty() || rel.split(['/', '\\']).any(|seg| seg == "..") {
        return None;
    }
    let src = storage_dir.join(rel);
    if !tokio::fs::metadata(&src)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return None;
    }
    let dir_name = format!(
        "{}_{}",
        name.replace(['/', '\\'], "_"),
        author.replace(['/', '\\'], "_")
    );
    let target_dir = storage_dir.join("data").join(namespace).join(&dir_name);
    let filename = src.file_name()?;
    let target = target_dir.join(filename);
    if src == target {
        return None;
    }
    if tokio::fs::create_dir_all(&target_dir).await.is_err() {
        return None;
    }
    let moved = if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        match tokio::fs::copy(&src, &target).await {
            Ok(_) => tokio::fs::remove_file(&src).await,
            Err(e) => Err(e),
        }
    } else {
        match tokio::fs::rename(&src, &target).await {
            Ok(()) => Ok(()),
            Err(_) => match tokio::fs::copy(&src, &target).await {
                Ok(_) => tokio::fs::remove_file(&src).await,
                Err(e) => Err(e),
            },
        }
    };
    if moved.is_err() {
        return None;
    }
    Some(format!(
        "storage/data/{namespace}/{dir_name}/{}",
        filename.to_string_lossy()
    ))
}

fn local_book_display_meta(
    file_name: &str,
    ext: &str,
    imported: &crate::service::local_book::ImportedBook,
) -> (String, String) {
    let (file_name_title, file_name_author) =
        crate::service::local_book::analyze_name_author(file_name);
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_string());
    let name = if ext == "txt" {
        if !file_name_title.is_empty() {
            file_name_title
        } else if !imported.meta.title.is_empty() {
            imported.meta.title.clone()
        } else {
            stem
        }
    } else if !imported.meta.title.is_empty() {
        imported.meta.title.clone()
    } else if !file_name_title.is_empty() {
        file_name_title
    } else {
        stem
    };
    let author = if !imported.meta.author.is_empty() {
        imported.meta.author.clone()
    } else {
        file_name_author
    };
    (name, author)
}

pub(super) fn is_loc_book_file_chapter(chapter_url: &str) -> bool {
    let Some((book_part, _)) = chapter_url.rsplit_once('#') else {
        return false;
    };
    if book_part.starts_with("storage/") {
        return true;
    }
    let lower = book_part.to_lowercase();
    crate::service::local_book::SUPPORTED_EXTENSIONS
        .iter()
        .any(|e| lower.ends_with(&format!(".{e}")))
}

fn loc_file_owned_by(storage_dir: &Path, ns: &str, book_part: &str) -> bool {
    let ns = if ns.is_empty() { "default" } else { ns };
    let Some(abs) = resolve_loc_book_file(storage_dir, book_part) else {
        return false;
    };
    let root = storage_dir.join("data").join(ns);
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    abs.starts_with(&root)
}

pub(super) async fn get_book_toc_loc_book(
    state: &AppState,
    namespace: &str,
    req_url: &str,
    toc_rule: &str,
) -> Option<Response> {
    let books = state.storage.list_books(namespace).await.ok()?;
    let book = books
        .iter()
        .find(|b| b.origin == "loc_book" && !b.book_url.is_empty() && b.book_url == req_url)
        .or_else(|| {
            books
                .iter()
                .find(|b| b.origin == "loc_book" && !b.book_url.is_empty())
        })
        .or_else(|| books.iter().find(|b| b.origin == "loc_book"))?;
    let book_url = &book.book_url;
    if let Ok(chapters) = state
        .storage
        .list_chapters_with_word_count_ns(namespace, book_url)
        .await
    {
        if !chapters.is_empty() {
            return toc_json_from_chapters(book_url, &chapters);
        }
    }
    if !book_url.starts_with("storage/") {
        return None;
    }
    let storage_dir = state.storage.config.storage_dir();
    let book_url_owned = book_url.clone();
    let toc_mode = if crate::service::local_book::is_epub_toc_mode(toc_rule) {
        toc_rule.to_string()
    } else {
        book.toc_url.clone()
    };
    let split_long = book.split_long_chapter;
    let imported = tokio::task::spawn_blocking(move || {
        let path = resolve_loc_book_file(&storage_dir, &book_url_owned)?;
        crate::service::local_book::parse_loc_book_path(&path, &[], &toc_mode, split_long).ok()
    })
    .await
    .ok()??;
    let list: Vec<LocalChapterItem> = imported
        .chapters
        .iter()
        .enumerate()
        .map(|(i, c)| LocalChapterItem {
            title: c.title.clone(),
            url: format!("{book_url}#{i}"),
            is_volume: false,
            index: i as i64,
            chapter_word_count: c.content.chars().count() as i64,
        })
        .collect();
    Some(ReturnData::ok_typed(list).into_response())
}

pub(super) async fn get_book_toc_file(
    state: &AppState,
    ns: &str,
    book_url: &str,
) -> Option<Response> {
    if let Ok(chapters) = state
        .storage
        .list_chapters_with_word_count_ns(ns, book_url)
        .await
    {
        if !chapters.is_empty() {
            return toc_json_from_chapters(book_url, &chapters);
        }
    }
    let storage_dir = state.storage.config.storage_dir();
    let ns_owned = ns.to_string();
    let book_url_owned = book_url.to_string();
    let path = tokio::task::spawn_blocking(move || {
        if !loc_file_owned_by(&storage_dir, &ns_owned, &book_url_owned) {
            return None;
        }
        resolve_storage_path(&storage_dir, &book_url_owned)
            .or_else(|| resolve_loc_book_file(&storage_dir, &book_url_owned))
    })
    .await
    .ok()??;
    let shelf = state.storage.find_book(ns, book_url).await.ok().flatten();
    let toc_mode = shelf
        .as_ref()
        .map(|b| b.toc_url.clone())
        .unwrap_or_default();
    let split_long = shelf
        .as_ref()
        .map(|b| b.split_long_chapter)
        .unwrap_or(false);
    let user_rules = txt_toc_rule_regexes(state, ns).await;
    let imported = tokio::task::spawn_blocking(move || {
        crate::service::local_book::parse_loc_book_path(&path, &user_rules, &toc_mode, split_long)
            .ok()
    })
    .await
    .ok()??;
    let list: Vec<LocalChapterItem> = imported
        .chapters
        .iter()
        .enumerate()
        .map(|(i, c)| LocalChapterItem {
            title: c.title.clone(),
            url: format!("{book_url}#{i}"),
            is_volume: false,
            index: i as i64,
            chapter_word_count: c.content.chars().count() as i64,
        })
        .collect();
    Some(ReturnData::ok_typed(list).into_response())
}

fn toc_json_from_chapters(book_url: &str, chapters: &[(i64, String, i64)]) -> Option<Response> {
    let list: Vec<LocalChapterItem> = chapters
        .iter()
        .map(|(idx, title, wc)| LocalChapterItem {
            title: title.clone(),
            url: format!("{book_url}#{idx}"),
            is_volume: false,
            index: *idx,
            chapter_word_count: *wc,
        })
        .collect();
    Some(ReturnData::ok_typed(list).into_response())
}

pub(super) async fn get_book_content_file(
    state: &AppState,
    ns: &str,
    chapter_url: &str,
    epub_content: bool,
) -> Option<Response> {
    let (book_part, idx_part) = chapter_url.rsplit_once('#')?;
    let index: i64 = idx_part.parse().ok()?;
    let is_epub = epub_content && crate::service::local_book::file_ext(book_part) == "epub";
    let storage_dir = state.storage.config.storage_dir();
    let ns_owned = ns.to_string();
    let book_part_owned = book_part.to_string();
    let owned = tokio::task::spawn_blocking(move || {
        loc_file_owned_by(&storage_dir, &ns_owned, &book_part_owned)
    })
    .await
    .unwrap_or(false);
    if !owned {
        return None;
    }
    if let Ok(Some(content)) = state
        .storage
        .get_chapter_content(ns, book_part, index)
        .await
    {
        if !content.trim().is_empty() {
            let content = if is_epub {
                wrap_epub_html(&content)
            } else {
                content
            };
            return Some(ReturnData::ok_typed(ContentPayload { content }).into_response());
        }
    }
    let shelf = state.storage.find_book(ns, book_part).await.ok().flatten();
    let toc_mode = shelf
        .as_ref()
        .map(|b| b.toc_url.clone())
        .unwrap_or_default();
    let split_long = shelf
        .as_ref()
        .map(|b| b.split_long_chapter)
        .unwrap_or(false);
    let user_rules = txt_toc_rule_regexes(state, ns).await;
    let storage_dir = state.storage.config.storage_dir();
    let book_part_owned = book_part.to_string();
    let imported = tokio::task::spawn_blocking(move || {
        let path = resolve_loc_book_file(&storage_dir, &book_part_owned)?;
        crate::service::local_book::parse_loc_book_path(&path, &user_rules, &toc_mode, split_long)
            .ok()
    })
    .await
    .ok()??;
    let content = imported.chapters.get(index as usize)?.content.clone();
    let content = if is_epub {
        wrap_epub_html(&content)
    } else {
        content
    };
    Some(ReturnData::ok_typed(ContentPayload { content }).into_response())
}

pub(super) async fn get_book_toc_local(
    state: &AppState,
    ns: &str,
    book_url: &str,
) -> Option<Response> {
    let chapters = state
        .storage
        .list_chapters_with_word_count_ns(ns, book_url)
        .await
        .ok()?;
    let list: Vec<LocalChapterItem> = chapters
        .iter()
        .map(|(idx, title, wc)| LocalChapterItem {
            title: title.clone(),
            url: format!("{book_url}/{idx}"),
            is_volume: false,
            index: *idx,
            chapter_word_count: *wc,
        })
        .collect();
    Some(ReturnData::ok_typed(list).into_response())
}

pub(super) async fn get_book_content_local(
    state: &AppState,
    ns: &str,
    chapter_url: &str,
    epub_content: bool,
) -> Option<Response> {
    let rest = chapter_url.trim_start_matches("local://");
    let (book_id, idx_str) = rest.rsplit_once('/')?;
    let index: i64 = idx_str.parse().ok()?;
    let book_url = format!("local://{book_id}");
    let is_epub = if epub_content {
        crate::service::local_book::file_ext(
            &resolve_local_book_source_file(state, ns, &book_url)
                .await
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ) == "epub"
    } else {
        false
    };
    let content = state
        .storage
        .get_chapter_content(ns, &book_url, index)
        .await
        .ok()??;
    let content = if is_epub {
        wrap_epub_html(&content)
    } else {
        content
    };
    Some(ReturnData::ok_typed(ContentPayload { content }).into_response())
}

fn wrap_epub_html(text: &str) -> String {
    let html = text
        .lines()
        .map(|l| format!("<p>{l}</p>"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("<html><body>{html}</body></html>")
}

async fn resolve_local_book_source_file(
    state: &AppState,
    ns: &str,
    book_url: &str,
) -> Option<PathBuf> {
    if let Some(file_id) = book_url.strip_prefix("local://") {
        if file_id.is_empty()
            || file_id.contains('/')
            || file_id.contains('\\')
            || file_id.contains("..")
        {
            return None;
        }
        let user_data = state.storage.config.storage_dir().join("data").join(ns);
        for sub in ["local_files", "opds_files"] {
            let dir = user_data.join(sub);
            for ext in crate::service::local_book::SUPPORTED_EXTENSIONS {
                let p = dir.join(format!("{file_id}.{ext}"));
                if tokio::fs::metadata(&p)
                    .await
                    .map(|m| m.is_file())
                    .unwrap_or(false)
                {
                    return Some(p);
                }
            }
        }
        None
    } else {
        let storage_dir = state.storage.config.storage_dir();
        let book_url = book_url.to_string();
        tokio::task::spawn_blocking(move || resolve_loc_book_file(&storage_dir, &book_url))
            .await
            .ok()
            .flatten()
    }
}
