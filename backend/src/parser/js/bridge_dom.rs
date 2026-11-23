//! DOM / HTML 解析桥接：`java.setContent`/`getString`/`getElements`/`htmlFormat`
//! 与 `org.jsoup.Jsoup.parse`（Document / Elements / Element shim，scraper 后端）。

use std::sync::Arc;

use boa_engine::object::builtins::JsArray;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::{Context, JsArgs, JsResult, JsString, JsValue, NativeFunction};

use crate::parser::css_chain::{
    css_chain, parse_html_document_cached, parse_html_fragment_cached, parse_selector_cached,
};

use super::runtime::{js_native_error, js_value_to_string, JsBridgeInner};

/// `java.setContent(html)`：设置当前解析文档（后续 getString/getElements 的解析源）
pub(super) fn java_set_content(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let html = js_value_to_string(args.get_or_undefined(0), context);
    *inner.doc.lock().unwrap_or_else(|e| e.into_inner()) = Some(html);
    Ok(JsValue::undefined())
}

/// 当前文档（未 setContent → 明确错误提示）
fn current_doc(inner: &JsBridgeInner) -> JsResult<String> {
    inner
        .doc
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| js_native_error("尚未设置文档内容：请先调用 java.setContent(html)"))
}

/// 规则末段是否为属性/文本提取器（与 css_chain::is_attr_extractor 对齐）
fn rule_ends_with_extractor(rule: &str) -> bool {
    rule.split('@')
        .next_back()
        .map(|s| {
            matches!(
                s.trim(),
                "text"
                    | "textNodes"
                    | "ownText"
                    | "html"
                    | "all"
                    | "href"
                    | "src"
                    | "value"
                    | "data-src"
                    | "data-original"
                    | "data-url"
            )
        })
        .unwrap_or(false)
}

/// `java.getString(rule)`：对已存文档用 css_chain 规则求值，返回首个结果文本。
/// 规则末段是 `@text/@href` 等提取器时返回提取值；否则结果是元素 outerHTML，
/// 转文本（对齐 legado getString 的 getText 语义）；无匹配返回空串。
pub(super) fn java_get_string(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let rule = js_value_to_string(args.get_or_undefined(0), context);
    let doc = current_doc(inner)?;
    let results = css_chain(&rule, &doc);
    let text = match results.first() {
        Some(first) => {
            if rule_ends_with_extractor(&rule) {
                first.clone()
            } else {
                // 元素 HTML → 文本（对齐 search::field 语义；经同线程 DOM 片段缓存复用）
                let f = parse_html_fragment_cached(first);
                let t = f
                    .root_element()
                    .text()
                    .collect::<String>()
                    .trim()
                    .to_string();
                if t.is_empty() {
                    first.clone()
                } else {
                    t
                }
            }
        }
        None => String::new(),
    };
    Ok(JsValue::from(JsString::from(text)))
}

/// `java.getElements(rule)`：对已存文档用 css_chain 规则求值。
/// - 规则带 `@` 提取器（@text/@href 等）→ 返回提取值字符串数组（原语义）
/// - 纯选择器规则 → 返回元素对象数组（jsoup 语义：`el.select(css)`/`el.text()`/`el.attr(name)`）
pub(super) fn java_get_elements(
    inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let rule = js_value_to_string(args.get_or_undefined(0), context);
    let doc = current_doc(inner)?;
    let results = css_chain(&rule, &doc);
    if rule_ends_with_extractor(&rule) {
        let arr = JsArray::from_iter(
            results
                .into_iter()
                .map(|s| JsValue::from(JsString::from(s))),
            context,
        );
        return Ok(arr.into());
    }
    // 元素对象数组（jsoup Elements：select/text/attr/first/get/size/eachAttr/eachText/html/val）
    jsoup_elements_from_htmls(results, context)
}

/// java.htmlFormat(str)：HTML → 纯文本（保留图片占位/换行语义，同正文清洗）
pub(super) fn java_html_format(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(
        crate::service::book::html_content_to_text(&s),
    )))
}

// ---- org.jsoup shim（scraper 后端） ----

/// org.jsoup.Jsoup.parse(html)：Document 对象（select/text/title/html/toString）
pub(super) fn jsoup_parse(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let html = js_value_to_string(args.get_or_undefined(0), context);
    jsoup_document(&html, context)
}

/// Document 对象
fn jsoup_document(html: &str, context: &mut Context) -> JsResult<JsValue> {
    let html = html.to_string();
    let mut doc = ObjectInitializer::new(context);
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let css = js_value_to_string(args.get_or_undefined(0), ctx);
                    jsoup_select(&h, &css, ctx)
                })
            },
            JsString::from("select"),
            1,
        );
    }
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    let parsed = parse_html_document_cached(&h);
                    let txt = parsed.root_element().text().collect::<String>();
                    Ok(JsValue::from(JsString::from(txt)))
                })
            },
            JsString::from("text"),
            0,
        );
    }
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    let parsed = parse_html_document_cached(&h);
                    let title = parse_selector_cached("title")
                        .and_then(|sel| parsed.select(&sel).next())
                        .map(|e| e.text().collect::<String>())
                        .unwrap_or_default();
                    Ok(JsValue::from(JsString::from(title)))
                })
            },
            JsString::from("title"),
            0,
        );
    }
    {
        let h = html.clone();
        doc.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(h.clone())))
                })
            },
            JsString::from("toString"),
            0,
        );
    }
    Ok(doc.build().into())
}

/// Elements 选择（css 选择器 → 元素对象数组 + jsoup 方法）
fn jsoup_select(html: &str, css: &str, context: &mut Context) -> JsResult<JsValue> {
    let parsed = parse_html_fragment_cached(html);
    let Some(selector) = parse_selector_cached(css) else {
        return Ok(JsArray::new(context).into()); // 非法选择器 → 空 Elements（不抛错）
    };
    let htmls: Vec<String> = parsed.select(&selector).map(|e| e.html()).collect();
    jsoup_elements_from_htmls(htmls, context)
}

/// 由元素 HTML 列表构建 jsoup Elements（数组 + attr/text/first/get/size/eachAttr/eachText/html/val）
fn jsoup_elements_from_htmls(htmls: Vec<String>, context: &mut Context) -> JsResult<JsValue> {
    let arr = JsArray::new(context);
    for h in &htmls {
        let el = jsoup_element(h, context)?;
        arr.push(el, context)?;
    }
    let htmls = Arc::new(htmls);
    // attr(name)：首元素属性
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("attr"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx);
                    Ok(JsValue::from(JsString::from(first_attr(&htmls, &name))))
                })
            })
            .name("attr")
            .length(1)
            .build(),
            true,
            context,
        )?;
    }
    // text()：首元素文本
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("text"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    Ok(JsValue::from(JsString::from(first_text(&htmls))))
                })
            })
            .name("text")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // first()：首元素对象（无 → null）
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("first"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, ctx| match htmls.first() {
                    Some(h) => jsoup_element(h, ctx),
                    None => Ok(JsValue::null()),
                })
            })
            .name("first")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // get(i)：第 i 个元素对象（越界 → null）
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("get"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let i = args.get_or_undefined(0).to_i32(ctx).unwrap_or(0);
                    match htmls.get(i.max(0) as usize) {
                        Some(h) => jsoup_element(h, ctx),
                        None => Ok(JsValue::null()),
                    }
                })
            })
            .name("get")
            .length(1)
            .build(),
            true,
            context,
        )?;
    }
    // size()：数量
    {
        let n = htmls.len() as i32;
        arr.set(
            JsString::from("size"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| Ok(JsValue::from(n)))
            })
            .name("size")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // eachAttr(name)：各元素属性值数组
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("eachAttr"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx);
                    let vals: Vec<JsValue> = htmls
                        .iter()
                        .map(|h| JsValue::from(JsString::from(attr_of(h, &name))))
                        .collect();
                    Ok(JsArray::from_iter(vals, ctx).into())
                })
            })
            .name("eachAttr")
            .length(1)
            .build(),
            true,
            context,
        )?;
    }
    // eachText()：各元素文本数组
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("eachText"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, ctx| {
                    let vals: Vec<JsValue> = htmls
                        .iter()
                        .map(|h| JsValue::from(JsString::from(text_of(h))))
                        .collect();
                    Ok(JsArray::from_iter(vals, ctx).into())
                })
            })
            .name("eachText")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // html()：首元素 innerHTML
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("html"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    Ok(JsValue::from(JsString::from(inner_html_of(
                        htmls.first().map(String::as_str).unwrap_or(""),
                    ))))
                })
            })
            .name("html")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // val()：首元素 value 属性/文本
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("val"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _ctx| {
                    let v = htmls
                        .first()
                        .map(|h| {
                            let a = attr_of(h, "value");
                            if a.is_empty() {
                                text_of(h)
                            } else {
                                a
                            }
                        })
                        .unwrap_or_default();
                    Ok(JsValue::from(JsString::from(v)))
                })
            })
            .name("val")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    // toString()/outerHtml()：全部元素 HTML 拼接
    {
        let htmls = Arc::clone(&htmls);
        arr.set(
            JsString::from("toString"),
            FunctionObjectBuilder::new(context.realm(), unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(htmls.join(""))))
                })
            })
            .name("toString")
            .length(0)
            .build(),
            true,
            context,
        )?;
    }
    Ok(arr.into())
}

/// jsoup 元素对象（select/attr/text/html/val/ownText/toString）
fn jsoup_element(html: &str, context: &mut Context) -> JsResult<JsValue> {
    let html = html.to_string();
    let mut el = ObjectInitializer::new(context);
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let css = js_value_to_string(args.get_or_undefined(0), ctx);
                    jsoup_select(&h, &css, ctx)
                })
            },
            JsString::from("select"),
            1,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, args, ctx| {
                    let name = js_value_to_string(args.get_or_undefined(0), ctx);
                    Ok(JsValue::from(JsString::from(attr_of(&h, &name))))
                })
            },
            JsString::from("attr"),
            1,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(text_of(&h))))
                })
            },
            JsString::from("text"),
            0,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(inner_html_of(&h))))
                })
            },
            JsString::from("html"),
            0,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    let v = attr_of(&h, "value");
                    Ok(JsValue::from(JsString::from(if v.is_empty() {
                        text_of(&h)
                    } else {
                        v
                    })))
                })
            },
            JsString::from("val"),
            0,
        );
    }
    {
        let h = html.clone();
        el.function(
            unsafe {
                NativeFunction::from_closure(move |_t, _a, _c| {
                    Ok(JsValue::from(JsString::from(h.clone())))
                })
            },
            JsString::from("toString"),
            0,
        );
    }
    Ok(el.build().into())
}

/// 元素 HTML → 首个元素属性值
fn attr_of(html: &str, name: &str) -> String {
    let parsed = parse_html_fragment_cached(html);
    parsed.root_element().attr(name).unwrap_or("").to_string()
}

/// 元素 HTML → 文本
fn text_of(html: &str) -> String {
    let parsed = parse_html_fragment_cached(html);
    parsed
        .root_element()
        .text()
        .collect::<String>()
        .trim()
        .to_string()
}

/// 元素 HTML → innerHTML
fn inner_html_of(html: &str) -> String {
    let parsed = parse_html_fragment_cached(html);
    parsed.root_element().inner_html()
}

/// 首个元素属性值（Elements.attr）
fn first_attr(htmls: &[String], name: &str) -> String {
    htmls.first().map(|h| attr_of(h, name)).unwrap_or_default()
}

/// 首个元素文本（Elements.text）
fn first_text(htmls: &[String]) -> String {
    htmls.first().map(|h| text_of(h)).unwrap_or_default()
}
