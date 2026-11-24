//! 章节目录与正文 API：`get_chapter_list`、`get_book_content`、`get_chapter_list_by_rule` 及离线目录合成/阅读进度同步辅助

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::helpers::{resolve_loc_book_file, resolve_storage_path, txt_toc_rule_regexes};
use super::local::{
    get_book_content_file, get_book_content_local, get_book_toc_file, get_book_toc_loc_book,
    get_book_toc_local, is_loc_book_file_chapter, ContentPayload, LocalChapterItem,
};
use crate::api::auth::resolve_namespace;
use crate::api::router::{now_millis, param_of, toc_cache_ttl_ms, AppState, ReturnData};
use crate::api::sources::{bump_source_use, resolve_book_source, resolve_book_source_for_url};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioContentPayload {
    audio_url: String,
    content_type: &'static str,
}

#[derive(serde::Serialize)]
struct ImagesContentPayload {
    images: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadContentPayload {
    download_url: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoContentPayload {
    video_url: String,
}

#[derive(serde::Serialize)]
struct LocalRuleChaptersPayload {
    book: crate::model::Book,
    chapters: Vec<LocalChapterItem>,
}

/// 按当前用户启用的净化规则处理章节正文（`get_replace_rules` 走内存 LRU 缓存，零 SQL 开销）
pub(super) async fn purify_book_content(
    state: &AppState,
    ns: &str,
    content: String,
    use_replace_rule: bool,
    book_name: Option<&str>,
    source_url: Option<&str>,
) -> String {
    if !use_replace_rule || content.is_empty() {
        return content;
    }
    let Ok(rules) = state.storage.get_replace_rules(ns).await else {
        return content;
    };
    if rules.is_empty() {
        return content;
    }
    crate::service::book::apply_replace_rules(
        &content,
        &rules,
        crate::service::book::PurifyTarget::Content,
        book_name,
        source_url,
    )
}

/// 按当前用户启用的标题净化规则处理目录标题（仅当至少有一条启用了 `scopeTitle` 规则时遍历）
async fn purify_chapter_titles(
    state: &AppState,
    ns: &str,
    chapters: &mut [crate::model::book_chapter::BookChapter],
    use_replace_rule: bool,
    book_name: Option<&str>,
    source_url: Option<&str>,
) {
    if !use_replace_rule || chapters.is_empty() {
        return;
    }
    let Ok(rules) = state.storage.get_replace_rules(ns).await else {
        return;
    };
    if !rules
        .iter()
        .any(|r| r.enabled && r.scope_title && !r.find.is_empty())
    {
        return;
    }
    for ch in chapters.iter_mut() {
        ch.title = crate::service::book::apply_replace_rules(
            &ch.title,
            &rules,
            crate::service::book::PurifyTarget::Title,
            book_name,
            source_url,
        );
    }
}

/// GET/POST `/reader3/getChapterList`：章节目录（支持缓存命中、质量门与离线合成兜底）
pub async fn get_chapter_list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let url_param = param_of(&params, body_json.as_ref(), "url");
    let mut toc_url = param_of(&params, body_json.as_ref(), "tocUrl");
    if toc_url.is_empty() {
        toc_url = url_param.clone();
    }
    if toc_url.is_empty() && !url_param.is_empty() {
        if let Ok(Some(b)) = state.storage.find_book(&namespace, &url_param).await {
            if !b.toc_url.is_empty() {
                toc_url = b.toc_url.clone();
            }
        }
    }
    let refresh = param_of(&params, body_json.as_ref(), "refresh")
        .parse::<i64>()
        .unwrap_or(0);
    if refresh <= 0 {
        if let Ok(Some(cached)) = state
            .storage
            .get_toc_cache(&namespace, &toc_url, toc_cache_ttl_ms())
            .await
        {
            if let Ok(mut chapters) =
                serde_json::from_str::<Vec<crate::model::book_chapter::BookChapter>>(&cached)
            {
                calibrate_total_from_cached_toc(&state, &namespace, &url_param, chapters.len())
                    .await;
                let shelf_meta = if !url_param.is_empty() {
                    state
                        .storage
                        .find_book(&namespace, &url_param)
                        .await
                        .ok()
                        .flatten()
                } else {
                    None
                };
                purify_chapter_titles(
                    &state,
                    &namespace,
                    &mut chapters,
                    shelf_meta.as_ref().is_none_or(|b| b.use_replace_rule),
                    shelf_meta.as_ref().map(|b| b.name.as_str()),
                    shelf_meta.as_ref().map(|b| b.origin.as_str()),
                )
                .await;
                return ReturnData::ok_typed(chapters).into_response();
            }
        }
    }
    if !url_param.is_empty() && toc_url == url_param {
        let bs_early = param_of(&params, body_json.as_ref(), "bookSource");
        if let Some(source) =
            resolve_book_source_for_url(&state, &namespace, &bs_early, &url_param).await
        {
            toc_url = crate::service::book::resolve_toc_url(&namespace, &source, &url_param).await;
        }
    }
    if toc_url.is_empty() {
        return Json(ReturnData::err("请输入书籍链接")).into_response();
    }
    if toc_url.starts_with("local://") {
        let book_id = toc_url
            .trim_start_matches("local://")
            .split('/')
            .next()
            .unwrap_or("");
        if let Some(ret) =
            get_book_toc_local(&state, &namespace, &format!("local://{book_id}")).await
        {
            return ret;
        }
        return Json(ReturnData::err("本地书籍源文件不存在")).into_response();
    }
    if crate::service::local_book::SUPPORTED_EXTENSIONS
        .iter()
        .any(|e| toc_url.to_lowercase().ends_with(&format!(".{e}")))
    {
        if let Some(ret) = get_book_toc_file(&state, &namespace, &toc_url).await {
            return ret;
        }
        return Json(ReturnData::err("本地书籍源文件不存在")).into_response();
    }
    if toc_url.starts_with("storage/")
        || toc_url.starts_with("spin")
        || toc_url.contains("(?")
        || toc_url.contains("序章")
        || toc_url.contains("楔子")
    {
        let req_url = if url_param.is_empty() {
            toc_url.clone()
        } else {
            url_param.clone()
        };
        if let Some(ret) = get_book_toc_loc_book(&state, &namespace, &req_url, &toc_url).await {
            return ret;
        }
        return Json(ReturnData::err("本地书籍源文件不存在")).into_response();
    }
    if refresh <= 0 {
        if let Ok(Some(cached)) = state
            .storage
            .get_toc_cache(&namespace, &toc_url, toc_cache_ttl_ms())
            .await
        {
            if let Ok(mut chapters) =
                serde_json::from_str::<Vec<crate::model::book_chapter::BookChapter>>(&cached)
            {
                calibrate_total_from_cached_toc(&state, &namespace, &url_param, chapters.len())
                    .await;
                let shelf_meta = if !url_param.is_empty() {
                    state
                        .storage
                        .find_book(&namespace, &url_param)
                        .await
                        .ok()
                        .flatten()
                } else {
                    None
                };
                purify_chapter_titles(
                    &state,
                    &namespace,
                    &mut chapters,
                    shelf_meta.as_ref().is_none_or(|b| b.use_replace_rule),
                    shelf_meta.as_ref().map(|b| b.name.as_str()),
                    shelf_meta.as_ref().map(|b| b.origin.as_str()),
                )
                .await;
                return ReturnData::ok_typed(chapters).into_response();
            }
        }
    }
    let bs_param = param_of(&params, body_json.as_ref(), "bookSource");
    let Some(source) = resolve_book_source_for_url(&state, &namespace, &bs_param, &toc_url).await
    else {
        return Json(ReturnData::err("未配置书源")).into_response();
    };
    let shelf_for_write = state
        .storage
        .find_book(&namespace, &url_param)
        .await
        .ok()
        .flatten();
    let cache_book_url = shelf_for_write
        .as_ref()
        .map(|b| b.book_url.clone())
        .filter(|u| !u.is_empty())
        .or_else(|| (!url_param.is_empty()).then(|| url_param.clone()))
        .unwrap_or_else(|| toc_url.clone());

    match crate::service::book::analyze_toc(
        &namespace,
        &toc_url,
        &source,
        20,
        shelf_for_write.as_ref().map(|b| b.name.as_str()),
        url_param.as_str(),
    )
    .await
    {
        Ok(chapters) => {
            let prev_total = shelf_for_write
                .as_ref()
                .map(|b| b.total_chapter_num)
                .unwrap_or(0);
            let cached_chapters = match shelf_for_write.as_ref() {
                Some(b) => state
                    .storage
                    .count_book_chapters(&namespace, &b.book_url)
                    .await
                    .unwrap_or(0),
                None => 0,
            };
            let looks_partial = !crate::storage::plausible_chapter_total(
                chapters.len() as i64,
                prev_total,
                cached_chapters,
            );
            if looks_partial {
                tracing::warn!(
                    "目录疑似残缺({} 章 vs 书架 {prev_total} / 已缓存 {cached_chapters}), 跳过缓存与章数回写 [{toc_url}]",
                    chapters.len(),
                );
            }
            if chapters.is_empty() || looks_partial {
                let reason = if chapters.is_empty() {
                    "空目录".to_string()
                } else {
                    format!(
                        "残缺 {} 章 vs 书架 {prev_total} / 已缓存 {cached_chapters}",
                        chapters.len()
                    )
                };
                if let Some(cached_toc) = toc_from_cached_chapters(
                    &state,
                    &namespace,
                    &cache_book_url,
                    &toc_url,
                    &reason,
                    shelf_for_write.as_ref(),
                )
                .await
                {
                    return ReturnData::ok_typed(cached_toc).into_response();
                }
            }
            if !chapters.is_empty() && !looks_partial {
                if let Ok(json) = serde_json::to_string(&chapters) {
                    let _ = state
                        .storage
                        .cache_toc(&namespace, &toc_url, &toc_url, &json)
                        .await;
                }
            }
            let calibrated_total = crate::storage::chapter_total_writeback(
                crate::storage::TocWitness::Fetched,
                chapters.len() as i64,
                prev_total,
                cached_chapters,
            );
            if let Some(shelf) = shelf_for_write.as_ref() {
                let latest = chapters.last().map(|c| c.title.clone());
                let mut patch = serde_json::Map::new();
                if let Some(total) = calibrated_total {
                    patch.insert("totalChapterNum".into(), json!(total));
                }
                patch.insert("lastCheckTime".into(), json!(now_millis()));
                patch.insert("lastCheckError".into(), Value::Null);
                if let Some(t) = latest {
                    patch.insert("latestChapterTitle".into(), json!(t));
                }
                let _ = state
                    .storage
                    .patch_book(&namespace, &shelf.book_url, &patch)
                    .await;
            }
            let mut chapters = chapters;
            purify_chapter_titles(
                &state,
                &namespace,
                &mut chapters,
                shelf_for_write.as_ref().is_none_or(|b| b.use_replace_rule),
                shelf_for_write.as_ref().map(|b| b.name.as_str()),
                Some(&source.book_source_url),
            )
            .await;
            ReturnData::ok_typed(chapters).into_response()
        }
        Err(e) => {
            if let Some(shelf) = shelf_for_write.as_ref() {
                let mut patch = serde_json::Map::new();
                patch.insert("lastCheckError".into(), json!(format!("{e:#}")));
                let _ = state
                    .storage
                    .patch_book(&namespace, &shelf.book_url, &patch)
                    .await;
            }
            tracing::error!("getChapterList 失败 [{toc_url}]: {e}");
            if let Some(cached_toc) = toc_from_cached_chapters(
                &state,
                &namespace,
                &cache_book_url,
                &toc_url,
                &format!("{e:#}"),
                shelf_for_write.as_ref(),
            )
            .await
            {
                return ReturnData::ok_typed(cached_toc).into_response();
            }
            Json(ReturnData::err("获取目录失败")).into_response()
        }
    }
}

/// GET/POST `/reader3/getBookContent`：章节正文（含本地书、离线缓存、音/漫/文/视四类书源）
pub async fn get_book_content(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut chapter_url = param_of(&params, body_json.as_ref(), "chapterUrl");
    if chapter_url.is_empty() {
        chapter_url = param_of(&params, body_json.as_ref(), "url");
    }
    let index_hint = param_of(&params, body_json.as_ref(), "index")
        .parse::<i64>()
        .ok()
        .filter(|i| *i >= 0);
    let cache_only = param_of(&params, body_json.as_ref(), "cache")
        .parse::<i64>()
        .unwrap_or(0)
        == 1;
    let refresh = param_of(&params, body_json.as_ref(), "refresh")
        .parse::<i64>()
        .unwrap_or(0);
    let epub_content = param_of(&params, body_json.as_ref(), "epubContent")
        .parse::<i64>()
        .unwrap_or(0)
        == 1;
    if chapter_url.is_empty() {
        return Json(ReturnData::err("请输入章节链接")).into_response();
    }
    if chapter_url.starts_with("local://") {
        if let Some(ret) =
            get_book_content_local(&state, &namespace, &chapter_url, epub_content).await
        {
            return ret;
        }
        return Json(ReturnData::err("本地书章节不存在")).into_response();
    }
    if chapter_url.contains('#') && is_loc_book_file_chapter(&chapter_url) {
        if let Some(ret) =
            get_book_content_file(&state, &namespace, &chapter_url, epub_content).await
        {
            return ret;
        }
        return Json(ReturnData::err("本地书章节不存在")).into_response();
    }
    if chapter_url.starts_with("cache://") {
        let Some((cached_book, hash)) = parse_cache_url(&chapter_url) else {
            return Json(ReturnData::err("章节链接无效")).into_response();
        };
        if let Ok(Some(content)) = state
            .storage
            .get_chapter_content(&namespace, cached_book, hash)
            .await
        {
            if !content.trim().is_empty() {
                if !cache_only {
                    save_reading_progress_if_shelf(
                        &state,
                        &namespace,
                        cached_book,
                        &chapter_url,
                        index_hint,
                    )
                    .await;
                }
                let cached_meta = state
                    .storage
                    .find_book(&namespace, cached_book)
                    .await
                    .ok()
                    .flatten();
                let content = purify_book_content(
                    &state,
                    &namespace,
                    content,
                    cached_meta.as_ref().is_none_or(|b| b.use_replace_rule),
                    cached_meta.as_ref().map(|b| b.name.as_str()),
                    cached_meta.as_ref().map(|b| b.origin.as_str()),
                )
                .await;
                return ReturnData::ok_typed(ContentPayload { content }).into_response();
            }
        }
        return Json(ReturnData::err("本地缓存里没有这一章, 源站当前不可用")).into_response();
    }
    let bs_param = param_of(&params, body_json.as_ref(), "bookSource");
    let Some(source) =
        resolve_book_source_for_url(&state, &namespace, &bs_param, &chapter_url).await
    else {
        return Json(ReturnData::err("书源不存在")).into_response();
    };
    let mut book_type = source.book_source_type.clamp(0, 4);
    let book_url = param_of(&params, body_json.as_ref(), "bookUrl");
    let mut shelf_book: Option<crate::model::book::Book> = None;
    if !book_url.is_empty() && !book_url.starts_with("local://") {
        if let Ok(Some(b)) = state.storage.find_book(&namespace, &book_url).await {
            if b.book_type > 0 {
                book_type = b.book_type.clamp(0, 4);
            }
            shelf_book = Some(b);
        }
    }
    let chapter_title_param = param_of(&params, body_json.as_ref(), "title");
    let chapter_title_ctx =
        (!chapter_title_param.is_empty()).then_some(chapter_title_param.as_str());
    let book_name_ctx = shelf_book.as_ref().map(|b| b.name.as_str());
    if !book_url.is_empty() && !book_url.starts_with("local://") {
        let mut ctx =
            crate::parser::rule::load_book_vars(&namespace, &source.book_source_url, &chapter_url);
        ctx.insert(
            crate::parser::rule::RK_CHAPTER_URL.to_string(),
            chapter_url.clone(),
        );
        if let Some(t) = chapter_title_ctx {
            ctx.insert(
                crate::parser::rule::RK_CHAPTER_TITLE.to_string(),
                t.to_string(),
            );
        }
        if let Some(b) = &shelf_book {
            ctx.insert(
                crate::parser::rule::RK_BOOK_NAME.to_string(),
                b.name.clone(),
            );
            if !b.author.is_empty() {
                ctx.insert(
                    crate::parser::rule::RK_BOOK_AUTHOR.to_string(),
                    b.author.clone(),
                );
            }
            ctx.insert(
                crate::parser::rule::RK_BOOK_URL.to_string(),
                b.book_url.clone(),
            );
        }
        if let Some((idx, toc_title, next_url)) =
            lookup_toc_chapter_ctx(&state, &namespace, &book_url, &chapter_url).await
        {
            ctx.insert(
                crate::parser::rule::RK_CHAPTER_INDEX.to_string(),
                idx.to_string(),
            );
            if let Some(t) = toc_title {
                ctx.entry(crate::parser::rule::RK_CHAPTER_TITLE.to_string())
                    .or_insert(t);
            }
            if let Some(n) = next_url {
                ctx.insert(crate::parser::rule::RK_NEXT_CHAPTER_URL.to_string(), n);
            }
        }
        crate::parser::rule::save_book_vars(
            &namespace,
            &source.book_source_url,
            &chapter_url,
            &ctx,
        );
    }
    let type_param = param_of(&params, body_json.as_ref(), "type");
    if let Ok(t) = type_param.parse::<i64>() {
        if (0..=4).contains(&t) {
            book_type = t;
        }
    }
    match book_type {
        1 => {
            return match crate::service::book::analyze_media_url(
                &namespace,
                &chapter_url,
                &source,
                chapter_title_ctx,
                book_name_ctx,
                &book_url,
            )
            .await
            {
                Ok(url) => {
                    bump_source_use(&state, &namespace, &source).await;
                    let content_type = crate::service::book::audio_content_type(&url);
                    ReturnData::ok_typed(AudioContentPayload {
                        audio_url: url,
                        content_type,
                    })
                    .into_response()
                }
                Err(e) => {
                    tracing::error!("getBookContent(音频) 失败 [{chapter_url}]: {e}");
                    Json(ReturnData::err("获取音频失败")).into_response()
                }
            };
        }
        2 => {
            return match crate::service::book::analyze_comic_images(
                &namespace,
                &chapter_url,
                &source,
                chapter_title_ctx,
                book_name_ctx,
                &book_url,
            )
            .await
            {
                Ok(images) if !images.is_empty() => {
                    bump_source_use(&state, &namespace, &source).await;
                    ReturnData::ok_typed(ImagesContentPayload { images }).into_response()
                }
                Ok(_) => Json(ReturnData::err("未提取到图片，请检查书源规则")).into_response(),
                Err(e) => {
                    tracing::error!("getBookContent(漫画) 失败 [{chapter_url}]: {e}");
                    Json(ReturnData::err("获取图片失败")).into_response()
                }
            };
        }
        3 => {
            return match crate::service::book::analyze_media_url(
                &namespace,
                &chapter_url,
                &source,
                chapter_title_ctx,
                book_name_ctx,
                &book_url,
            )
            .await
            {
                Ok(url) => {
                    bump_source_use(&state, &namespace, &source).await;
                    ReturnData::ok_typed(DownloadContentPayload { download_url: url })
                        .into_response()
                }
                Err(e) => {
                    tracing::error!("getBookContent(文件) 失败 [{chapter_url}]: {e}");
                    Json(ReturnData::err("获取下载链接失败")).into_response()
                }
            };
        }
        4 => {
            return match crate::service::book::analyze_media_url(
                &namespace,
                &chapter_url,
                &source,
                chapter_title_ctx,
                book_name_ctx,
                &book_url,
            )
            .await
            {
                Ok(url) => {
                    bump_source_use(&state, &namespace, &source).await;
                    ReturnData::ok_typed(VideoContentPayload { video_url: url }).into_response()
                }
                Err(e) => {
                    tracing::error!("getBookContent(视频) 失败 [{chapter_url}]: {e}");
                    Json(ReturnData::err("获取视频失败")).into_response()
                }
            };
        }
        _ => {}
    }

    if refresh <= 0 && !book_url.is_empty() && !book_url.starts_with("local://") {
        let idx = crate::util::md5::chapter_url_hash(&chapter_url);
        if let Ok(Some(content)) = state
            .storage
            .get_chapter_content(&namespace, &book_url, idx)
            .await
        {
            if !content.trim().is_empty() {
                if !cache_only {
                    save_reading_progress_if_shelf(
                        &state,
                        &namespace,
                        &book_url,
                        &chapter_url,
                        index_hint,
                    )
                    .await;
                }
                let content = purify_book_content(
                    &state,
                    &namespace,
                    content,
                    shelf_book.as_ref().is_none_or(|b| b.use_replace_rule),
                    book_name_ctx,
                    Some(&source.book_source_url),
                )
                .await;
                return ReturnData::ok_typed(ContentPayload { content }).into_response();
            }
        }
    }
    match crate::service::book::analyze_content(
        &namespace,
        &chapter_url,
        &source,
        5,
        chapter_title_ctx,
        book_name_ctx,
        &book_url,
    )
    .await
    {
        Ok(content) => {
            let _ = state
                .storage
                .record_path_outcome(
                    &namespace,
                    &source.book_source_url,
                    "content",
                    !content.trim().is_empty(),
                )
                .await;
            if !book_url.is_empty()
                && !book_url.starts_with("local://")
                && state.storage.config.cache_chapter_content
            {
                let idx = crate::util::md5::chapter_url_hash(&chapter_url);
                let title = param_of(&params, body_json.as_ref(), "title");
                let _ = state
                    .storage
                    .cache_chapter_content(&namespace, &book_url, idx, &title, &content)
                    .await;
                if !cache_only {
                    save_reading_progress_if_shelf(
                        &state,
                        &namespace,
                        &book_url,
                        &chapter_url,
                        index_hint,
                    )
                    .await;
                }
            }
            bump_source_use(&state, &namespace, &source).await;
            let content = purify_book_content(
                &state,
                &namespace,
                content,
                shelf_book.as_ref().is_none_or(|b| b.use_replace_rule),
                book_name_ctx,
                Some(&source.book_source_url),
            )
            .await;
            ReturnData::ok_typed(ContentPayload { content }).into_response()
        }
        Err(e) => {
            tracing::error!("getBookContent 失败 [{chapter_url}]: {e}");
            let _ = state
                .storage
                .record_path_outcome(&namespace, &source.book_source_url, "content", false)
                .await;
            Json(ReturnData::err("获取正文失败")).into_response()
        }
    }
}

/// GET/POST `/reader3/getChapterListByRule`：单页目录调试 / 本地书目录规则预览
pub async fn get_chapter_list_by_rule(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Option<axum::body::Bytes>,
) -> Response {
    let namespace = match resolve_namespace(&state, &params, &headers).await {
        Ok(ns) => ns,
        Err(ret) => return Json(ret).into_response(),
    };
    let body_json = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let body_book: Option<crate::model::Book> = body_json
        .as_ref()
        .and_then(|b| serde_json::from_value::<crate::model::Book>(b.clone()).ok())
        .filter(|b| {
            !b.name.is_empty() && crate::service::local_book::is_local_book(&b.book_url, &b.origin)
        });
    if let Some(bk) = body_book {
        if bk.origin.is_empty() {
            return Json(ReturnData::err("未找到书源信息")).into_response();
        }
        let ext = crate::service::local_book::file_ext(&bk.book_url);
        if !matches!(ext.as_str(), "txt" | "epub") && !bk.book_url.starts_with("local://") {
            return Json(ReturnData::err("非本地 txt/epub 书籍")).into_response();
        }
        let storage_dir = state.storage.config.storage_dir();
        let bk_url = bk.book_url.clone();
        let path = tokio::task::spawn_blocking(move || {
            resolve_loc_book_file(&storage_dir, &bk_url)
                .or_else(|| resolve_storage_path(&storage_dir, &bk_url))
        })
        .await
        .ok()
        .flatten();
        let entries: Vec<LocalChapterItem> = match path {
            Some(p) => {
                let user_rules = txt_toc_rule_regexes(&state, &namespace).await;
                let toc_url = bk.toc_url.clone();
                let split_long = bk.split_long_chapter;
                let parsed = tokio::task::spawn_blocking(move || {
                    crate::service::local_book::parse_loc_book_path(
                        &p,
                        &user_rules,
                        &toc_url,
                        split_long,
                    )
                })
                .await;
                match parsed {
                    Ok(Ok(imported)) => imported
                        .chapters
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            let entry_url = if bk.book_url.starts_with("local://") {
                                format!("{}/{}", bk.book_url, i)
                            } else {
                                format!("{}#{}", bk.book_url, i)
                            };
                            LocalChapterItem {
                                title: c.title.clone(),
                                url: entry_url,
                                is_volume: false,
                                index: i as i64,
                                chapter_word_count: c.content.chars().count() as i64,
                            }
                        })
                        .collect(),
                    Ok(Err(e)) => {
                        return Json(ReturnData::err(format!("解析失败：{e:#}"))).into_response();
                    }
                    Err(e) => {
                        return Json(ReturnData::err(format!("解析失败：{e}"))).into_response();
                    }
                }
            }
            None => match state
                .storage
                .list_chapters_with_word_count_ns(&namespace, &bk.book_url)
                .await
            {
                Ok(rows) if !rows.is_empty() => rows
                    .iter()
                    .map(|(idx, title, wc)| LocalChapterItem {
                        title: title.clone(),
                        url: format!("{}/{idx}", bk.book_url),
                        is_volume: false,
                        index: *idx,
                        chapter_word_count: *wc,
                    })
                    .collect(),
                _ => return Json(ReturnData::err("本地书文件不存在")).into_response(),
            },
        };
        return ReturnData::ok_typed(LocalRuleChaptersPayload {
            book: bk,
            chapters: entries,
        })
        .into_response();
    }

    let mut url = param_of(&params, body_json.as_ref(), "url");
    if url.is_empty() {
        url = param_of(&params, body_json.as_ref(), "chapterUrl");
    }
    if url.is_empty() {
        return Json(ReturnData::err("请输入目录链接")).into_response();
    }
    let bs_param = param_of(&params, body_json.as_ref(), "bookSource");
    if bs_param.is_empty() {
        return Json(ReturnData::err("书源不存在")).into_response();
    }
    let Some(source) = resolve_book_source(&state, &namespace, &bs_param).await else {
        return Json(ReturnData::err("书源不存在")).into_response();
    };
    match crate::service::book::parse_toc_page(&namespace, &url, &source, None, "").await {
        Ok(chapters) => ReturnData::ok_typed(chapters).into_response(),
        Err(e) => {
            tracing::error!("getChapterListByRule 失败 [{url}]: {e}");
            Json(ReturnData::err("获取目录失败")).into_response()
        }
    }
}

// ==================== 目录合成、章数校准与进度回写辅助 ====================

pub(super) fn synthesize_toc_from_cache(
    cached: &[(i64, String)],
    candidate_urls: &[String],
    book_url: &str,
) -> Vec<crate::model::book_chapter::BookChapter> {
    struct Row {
        hash: i64,
        title: String,
        url: String,
        num: Option<i64>,
        id: Option<i64>,
    }
    if cached.is_empty() {
        return Vec::new();
    }
    let url_by_hash: HashMap<i64, &str> = candidate_urls
        .iter()
        .map(|u| (crate::util::md5::chapter_url_hash(u), u.as_str()))
        .collect();
    let mut rows: Vec<Row> = cached
        .iter()
        .map(|(hash, title)| {
            let url = url_by_hash
                .get(hash)
                .map(|u| (*u).to_string())
                .unwrap_or_else(|| cache_url_of(book_url, *hash));
            Row {
                hash: *hash,
                num: chapter_num_of(title),
                id: url_tail_id(&url),
                url,
                title: title.clone(),
            }
        })
        .collect();
    let id_order_usable = rows.iter().all(|r| r.id.is_some()) && {
        let mut by_id: Vec<&Row> = rows.iter().collect();
        by_id.sort_by_key(|r| r.id);
        by_id
            .iter()
            .filter_map(|r| r.num)
            .collect::<Vec<i64>>()
            .windows(2)
            .all(|w| w[0] <= w[1])
    };
    if id_order_usable {
        rows.sort_by(|a, b| a.id.cmp(&b.id).then(a.hash.cmp(&b.hash)));
    } else {
        rows.sort_by(|a, b| {
            a.num
                .unwrap_or(i64::MAX)
                .cmp(&b.num.unwrap_or(i64::MAX))
                .then(a.hash.cmp(&b.hash))
        });
    }
    rows.into_iter()
        .enumerate()
        .map(|(i, r)| crate::model::book_chapter::BookChapter {
            title: r.title,
            url: r.url,
            tag: None,
            is_volume: false,
            index: i as i64,
        })
        .collect()
}

fn chapter_num_of(title: &str) -> Option<i64> {
    let rest = title.trim_start().strip_prefix('第')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn url_tail_id(url: &str) -> Option<i64> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let seg = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let stem = seg.split('.').next().unwrap_or("");
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

pub(super) fn cache_url_of(book_url: &str, hash: i64) -> String {
    format!("cache://{book_url}#{hash}")
}

pub(super) fn parse_cache_url(s: &str) -> Option<(&str, i64)> {
    let rest = s.strip_prefix("cache://")?;
    let (book_url, hash) = rest.rsplit_once('#')?;
    if book_url.is_empty() {
        return None;
    }
    hash.parse().ok().map(|h| (book_url, h))
}

async fn toc_from_cached_chapters(
    state: &AppState,
    ns: &str,
    book_url: &str,
    toc_url: &str,
    reason: &str,
    shelf: Option<&crate::model::book::Book>,
) -> Option<Vec<crate::model::book_chapter::BookChapter>> {
    let cached = state
        .storage
        .list_cached_chapter_headers(ns, book_url)
        .await
        .ok()?;
    if cached.is_empty() {
        return None;
    }
    let urls = state
        .storage
        .list_urls_under_prefix(ns, book_url)
        .await
        .unwrap_or_default();
    let chapters = synthesize_toc_from_cache(&cached, &urls, book_url);
    if chapters.is_empty() {
        return None;
    }
    tracing::warn!(
        "目录抓取失败({reason})，回落到 {} 章本地缓存 [{book_url}]",
        chapters.len()
    );
    if let Ok(json) = serde_json::to_string(&chapters) {
        let _ = state.storage.cache_toc(ns, toc_url, toc_url, &json).await;
    }
    if let Some(b) = shelf {
        if let Some(total) = crate::storage::chapter_total_writeback(
            crate::storage::TocWitness::CachedToc,
            chapters.len() as i64,
            b.total_chapter_num,
            0,
        ) {
            let mut patch = serde_json::Map::new();
            patch.insert("totalChapterNum".to_string(), json!(total));
            let _ = state.storage.patch_book(ns, &b.book_url, &patch).await;
        }
    }
    Some(chapters)
}

async fn calibrate_total_from_cached_toc(
    state: &AppState,
    namespace: &str,
    book_url: &str,
    toc_len: usize,
) {
    if book_url.is_empty() || toc_len == 0 {
        return;
    }
    let Ok(Some(shelf)) = state.storage.find_book(namespace, book_url).await else {
        return;
    };
    let cached = state
        .storage
        .count_book_chapters(namespace, &shelf.book_url)
        .await
        .unwrap_or(0);
    let Some(total) = crate::storage::chapter_total_writeback(
        crate::storage::TocWitness::CachedToc,
        toc_len as i64,
        shelf.total_chapter_num,
        cached,
    ) else {
        return;
    };
    let mut patch = serde_json::Map::new();
    patch.insert("totalChapterNum".into(), json!(total));
    patch.insert("lastCheckError".into(), Value::Null);
    let _ = state
        .storage
        .patch_book(namespace, &shelf.book_url, &patch)
        .await;
}

async fn save_reading_progress_if_shelf(
    state: &AppState,
    ns: &str,
    book_url: &str,
    chapter_url: &str,
    index_hint: Option<i64>,
) {
    let Ok(Some(book)) = state.storage.find_book(ns, book_url).await else {
        return;
    };
    let mut index = index_hint.unwrap_or(book.dur_chapter_index);
    let mut title: Option<String> = None;
    if let Ok(Some(toc_json)) = state
        .storage
        .get_toc_cache(ns, book_url, toc_cache_ttl_ms())
        .await
    {
        if let Ok(chapters) = serde_json::from_str::<Vec<Value>>(&toc_json) {
            for c in chapters {
                let url = c.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if url == chapter_url {
                    if let Some(i) = c.get("index").and_then(|v| v.as_i64()) {
                        index = i;
                    }
                    if let Some(t) = c.get("title").and_then(|v| v.as_str()) {
                        title = Some(t.to_string());
                    }
                    break;
                }
            }
        }
    }
    let _ = state
        .storage
        .update_book_progress(
            ns,
            book_url,
            title.as_deref(),
            index,
            book.dur_chapter_pos,
            now_millis(),
        )
        .await;
}

async fn lookup_toc_chapter_ctx(
    state: &AppState,
    ns: &str,
    book_url: &str,
    chapter_url: &str,
) -> Option<(i64, Option<String>, Option<String>)> {
    let toc_json = state
        .storage
        .get_toc_cache(ns, book_url, toc_cache_ttl_ms())
        .await
        .ok()
        .flatten()?;
    let chapters = serde_json::from_str::<Vec<Value>>(&toc_json).ok()?;
    for (i, c) in chapters.iter().enumerate() {
        let url = c.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url == chapter_url {
            let index = c.get("index").and_then(|v| v.as_i64()).unwrap_or(i as i64);
            let title = c.get("title").and_then(|v| v.as_str()).map(str::to_string);
            let next = chapters
                .get(i + 1)
                .and_then(|n| n.get("url"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            return Some((index, title, next));
        }
    }
    None
}
