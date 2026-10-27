//! EPUB/OPF 元数据解析（本地书导入前置 + OPDS 元数据）
//!
//! 解析 content.opf 的 OPF 2.0 元数据（对齐样本：identifier/title/creator/language/
//! date/description/publisher/subject 等全字段，不丢字段）

use serde::Serialize;

/// OPF 元数据（全字段）
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpfMeta {
    pub title: String,
    /// 作者列表（dc:creator，取全部）
    pub authors: Vec<String>,
    /// 作者（第一个，file-as 优先）
    pub author: String,
    pub language: Option<String>,
    pub publisher: Option<String>,
    pub published_at: Option<String>,
    pub description: Option<String>,
    pub subjects: Vec<String>,
    pub identifiers: Vec<String>,
    /// 封面路径（guide reference 或 manifest 中 cover）
    pub cover_href: Option<String>,
}

/// 解析 OPF XML（正则提取——OPF 结构固定，避免 XML 依赖）
pub fn parse_opf(xml: &str) -> OpfMeta {
    let mut meta = OpfMeta::default();

    // title（首个 dc:title）
    if let Some(t) = extract_tag(xml, "dc:title") {
        meta.title = decode_entities(&t);
    }
    // creators（全部）
    for m in extract_all_tags(xml, "dc:creator") {
        let text = decode_entities(&m);
        if !text.is_empty() {
            meta.authors.push(text.clone());
            if meta.author.is_empty() {
                meta.author = text;
            }
        }
    }
    // language
    meta.language = extract_tag(xml, "dc:language").map(|s| decode_entities(&s));
    // publisher
    meta.publisher = extract_tag(xml, "dc:publisher").map(|s| decode_entities(&s));
    // date
    meta.published_at = extract_tag(xml, "dc:date").map(|s| decode_entities(&s));
    // description
    meta.description = extract_tag(xml, "dc:description").map(|s| decode_entities(&s));
    // subjects（多个）
    meta.subjects = extract_all_tags(xml, "dc:subject")
        .iter()
        .map(|s| decode_entities(s))
        .filter(|s| !s.is_empty())
        .collect();
    // identifiers（多个——uuid 优先）
    meta.identifiers = extract_all_tags(xml, "dc:identifier")
        .iter()
        .map(|s| decode_entities(s))
        .filter(|s| !s.is_empty())
        .collect();

    // 封面：guide reference type=cover 的 href
    if let Some(href) = extract_attr(xml, "reference", "type", "cover", "href") {
        meta.cover_href = Some(href);
    }
    // 或 manifest 中 id=cover / properties=cover-image
    if meta.cover_href.is_none() {
        if let Some(href) = extract_attr(xml, "item", "id", "cover", "href") {
            meta.cover_href = Some(href);
        }
        if let Some(href) = extract_attr(xml, "item", "properties", "cover-image", "href") {
            meta.cover_href = Some(href);
        }
    }

    meta
}

/// 提取单个标签文本（跨行、含 CDATA）
pub(crate) fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    extract_all_tags(xml, tag).into_iter().next()
}

/// 提取全部同名标签文本
#[allow(clippy::while_let_loop)] // 显式 loop + let-else 便于多处 break，改成 while let 反而难读
pub(crate) fn extract_all_tags(xml: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    loop {
        let Some(start) = rest.find(&format!("<{tag}")) else {
            break;
        };
        let after = &rest[start..];
        let Some(gt) = after.find('>') else { break };
        // 跳过自闭合
        if after[..gt].ends_with('/') {
            rest = &after[gt + 1..];
            continue;
        }
        let close = format!("</{tag}>");
        let Some(end) = after[gt + 1..].find(&close) else {
            break;
        };
        let content = &after[gt + 1..gt + 1 + end];
        let text = content
            .trim()
            .trim_start_matches("<![CDATA[")
            .trim_end_matches("]]>")
            .trim()
            .to_string();
        if !text.is_empty() {
            out.push(text);
        }
        rest = &after[gt + 1 + end + close.len()..];
    }
    out
}

/// 提取带属性条件的标签 href：<item id="cover" href="...">
fn extract_attr(
    xml: &str,
    tag: &str,
    attr_key: &str,
    attr_val: &str,
    want: &str,
) -> Option<String> {
    let mut rest = xml;
    loop {
        let start = rest.find(&format!("<{tag}"))?;
        let after = &rest[start..];
        let gt = after.find('>')?;
        let tag_block = &after[..gt + 1];
        // 检查 attr_key="attr_val"（宽容：引号单双）
        let pattern_attr = format!("{attr_key}=\"{attr_val}\"");
        let pattern_attr2 = format!("{attr_key}='{attr_val}'");
        if tag_block.contains(&pattern_attr) || tag_block.contains(&pattern_attr2) {
            let want_pattern = format!("{want}=\"");
            let want_pattern2 = format!("{want}='");
            if let Some(i) = tag_block.find(&want_pattern) {
                let rest2 = &tag_block[i + want_pattern.len()..];
                return rest2.split('"').next().map(str::to_string);
            }
            if let Some(i) = tag_block.find(&want_pattern2) {
                let rest2 = &tag_block[i + want_pattern2.len()..];
                return rest2.split('\'').next().map(str::to_string);
            }
            return None;
        }
        rest = &after[gt + 1..];
    }
}

/// HTML/XML 实体解码（常用）
pub(crate) fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_opf_sample() {
        // 用户提供的测试样本（metadata.opf 全字段）
        let xml = r#"<?xml version='1.0' encoding='utf-8'?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="uuid_id" version="2.0">
    <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
        <dc:identifier opf:scheme="calibre" id="calibre_id">39</dc:identifier>
        <dc:identifier opf:scheme="uuid" id="uuid_id">088054ed-61ad-440c-8ba4-90f97710015b</dc:identifier>
        <dc:title>我的化身正在成为最终BOSS</dc:title>
        <dc:creator opf:file-as="汐尺" opf:role="aut">汐尺</dc:creator>
        <dc:contributor opf:file-as="calibre" opf:role="bkp">calibre (9.11.0) [https://calibre-ebook.com]</dc:contributor>
        <dc:date>2025-02-13T00:00:00+00:00</dc:date>
        <dc:description>平平无奇地生活了十多年后，某天夜里，姬明欢觉醒了一个允许他在现实世界"创建游戏角色"的异能。</dc:description>
        <dc:publisher>起点中文网</dc:publisher>
        <dc:identifier opf:scheme="QIDIAN_URL">https://www.qidian.com/book/1042464636</dc:identifier>
        <dc:identifier opf:scheme="QIDIAN">1042464636</dc:identifier>
        <dc:language>zho</dc:language>
        <dc:subject>轻小说</dc:subject>
        <dc:subject>原生幻想</dc:subject>
        <dc:subject>都市异能</dc:subject>
        <dc:subject>完本</dc:subject>
    </metadata>
    <guide>
        <reference type="cover" title="封面" href="cover.jpg"/>
    </guide>
</package>"#;
        let meta = parse_opf(xml);
        assert_eq!(meta.title, "我的化身正在成为最终BOSS");
        assert_eq!(meta.author, "汐尺");
        assert_eq!(meta.language.as_deref(), Some("zho"));
        assert_eq!(meta.publisher.as_deref(), Some("起点中文网"));
        assert!(meta
            .published_at
            .as_deref()
            .unwrap()
            .starts_with("2025-02-13"));
        assert!(meta.description.as_deref().unwrap().contains("姬明欢"));
        assert_eq!(meta.subjects.len(), 4, "多 subject 不丢");
        assert_eq!(meta.identifiers.len(), 4, "多 identifier 不丢");
        assert_eq!(meta.cover_href.as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn test_parse_opf_cdata() {
        let xml = r#"<package><metadata><dc:title><![CDATA[书名 <特殊> & 符号]]></dc:title></metadata></package>"#;
        let meta = parse_opf(xml);
        assert_eq!(meta.title, "书名 <特殊> & 符号");
    }
}
