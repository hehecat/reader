//! 书籍实体（兼容 legacy Book / bookshelf.json，JSON 字段 camelCase，全字段无丢失）

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// 书架书籍（books 表 ↔ bookshelf.json ↔ /reader3/getBookshelf 输出）
///
/// - serde：camelCase 与 legacy bookshelf.json / API 输出一致（legacy 全字段）
/// - sqlx：snake_case 与 books 表列名一致（`group` 为 SQLite 关键字 → 列名 `group_name`；
///   `order` 为 SQLite 关键字 → 列名 `order_num`）
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, FromRow)]
#[serde(default)]
pub struct Book {
    #[serde(rename = "bookUrl")]
    #[sqlx(rename = "book_url")]
    pub book_url: String,
    #[serde(rename = "tocUrl")]
    #[sqlx(rename = "toc_url")]
    pub toc_url: String,
    pub origin: String,
    #[serde(rename = "originName")]
    #[sqlx(rename = "origin_name")]
    pub origin_name: String,
    pub name: String,
    pub author: String,
    pub kind: Option<String>,
    /// 分类信息（用户修改）
    #[serde(rename = "customTag")]
    #[sqlx(rename = "custom_tag")]
    pub custom_tag: Option<String>,
    #[serde(rename = "coverUrl")]
    #[sqlx(rename = "cover_url")]
    pub cover_url: Option<String>,
    #[serde(rename = "customCoverUrl")]
    #[sqlx(rename = "custom_cover_url")]
    pub custom_cover_url: Option<String>,
    pub intro: Option<String>,
    /// 简介（用户修改）
    #[serde(rename = "customIntro")]
    #[sqlx(rename = "custom_intro")]
    pub custom_intro: Option<String>,
    /// 自定义字符集（仅本地书籍）
    pub charset: Option<String>,
    /// 书籍类型 @BookType（`type` 为 Rust 关键字 → 字段名 book_type）
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]
    pub book_type: i64,
    /// 自定义分组索引号（books 表列名 group_name）
    #[sqlx(rename = "group_name")]
    pub group: i64,
    /// 多分组 ID 列表（JSON 数组文本，如 `[1,3]`；主分组=group_name 与首项一致）。
    /// 内部字段：对外输出由 router 附加 `groupIds` 数组（见 get_bookshelf）
    #[serde(skip)]
    #[sqlx(rename = "group_ids")]
    pub group_ids: String,
    /// 最新章节标题
    #[serde(rename = "latestChapterTitle")]
    #[sqlx(rename = "latest_chapter_title")]
    pub latest_chapter_title: Option<String>,
    /// 最新章节标题更新时间
    #[serde(rename = "latestChapterTime")]
    #[sqlx(rename = "latest_chapter_time")]
    pub latest_chapter_time: i64,
    /// 最近一次更新书籍信息的时间
    #[serde(rename = "lastCheckTime")]
    #[sqlx(rename = "last_check_time")]
    pub last_check_time: i64,
    /// 最近一次发现新章节的数量
    #[serde(rename = "lastCheckCount")]
    #[sqlx(rename = "last_check_count")]
    pub last_check_count: i64,
    /// 书籍目录总数
    #[serde(rename = "totalChapterNum")]
    #[sqlx(rename = "total_chapter_num")]
    pub total_chapter_num: i64,
    #[serde(rename = "durChapterTitle")]
    #[sqlx(rename = "dur_chapter_title")]
    pub dur_chapter_title: Option<String>,
    #[serde(rename = "durChapterIndex")]
    #[sqlx(rename = "dur_chapter_index")]
    pub dur_chapter_index: i64,
    #[serde(rename = "durChapterPos")]
    #[sqlx(rename = "dur_chapter_pos")]
    pub dur_chapter_pos: i64,
    #[serde(rename = "durChapterTime")]
    #[sqlx(rename = "dur_chapter_time")]
    pub dur_chapter_time: i64,
    /// 字数
    #[serde(rename = "wordCount")]
    #[sqlx(rename = "word_count")]
    pub word_count: Option<String>,
    #[serde(rename = "canUpdate")]
    #[sqlx(rename = "can_update")]
    pub can_update: bool,
    /// 手动排序（order 为 SQLite 关键字 → 列名 order_num）
    #[sqlx(rename = "order_num")]
    pub order: i64,
    /// 书源排序
    #[serde(rename = "originOrder")]
    #[sqlx(rename = "origin_order")]
    pub origin_order: i64,
    /// 正文使用净化替换规则（默认开启）
    #[serde(rename = "useReplaceRule", default = "default_true")]
    #[sqlx(rename = "use_replace_rule")]
    pub use_replace_rule: bool,
    /// 自定义书籍变量（书源规则检索用）
    pub variable: Option<String>,
    /// 阅读配置（legacy ReadConfig 对象——存 JSON 文本）
    #[serde(rename = "readConfig")]
    #[sqlx(rename = "read_config")]
    pub read_config: Option<serde_json::Value>,
    /// 是否在书架
    #[serde(rename = "isInShelf")]
    #[sqlx(rename = "is_in_shelf")]
    pub is_in_shelf: bool,
    #[serde(rename = "lastCheckError")]
    #[sqlx(rename = "last_check_error")]
    pub last_check_error: Option<String>,
    /// 详情页 HTML 缓存
    #[serde(rename = "infoHtml")]
    #[sqlx(rename = "info_html")]
    pub info_html: Option<String>,
    /// 目录页 HTML 缓存
    #[serde(rename = "tocHtml")]
    #[sqlx(rename = "toc_html")]
    pub toc_html: Option<String>,
    /// 展示封面（legado 扩展）
    #[serde(rename = "displayCover")]
    #[sqlx(rename = "display_cover")]
    pub display_cover: Option<String>,
    /// 展示简介（legado 扩展）
    #[serde(rename = "displayIntro")]
    #[sqlx(rename = "display_intro")]
    pub display_intro: Option<String>,
    /// 本地 EPUB 标记（legado 扩展，Boolean）
    #[serde(rename = "localEpub")]
    #[sqlx(rename = "local_epub")]
    pub local_epub: bool,
    /// 是否拆分长章节（legado 扩展）
    #[serde(rename = "splitLongChapter")]
    #[sqlx(rename = "split_long_chapter")]
    pub split_long_chapter: bool,
    /// 语言（本地书/EPUB 元数据）
    pub language: Option<String>,
    /// 出版社
    pub publisher: Option<String>,
    /// 出版时间
    #[serde(rename = "publishedAt")]
    #[sqlx(rename = "published_at")]
    pub published_at: Option<String>,
    /// 用户命名空间（secure 模式用户名 / default）
    #[serde(skip)]
    #[sqlx(rename = "user_namespace")]
    pub user_namespace: String,
    /// 创建时间
    #[serde(rename = "createdAt")]
    #[sqlx(rename = "created_at")]
    pub created_at: i64,
    /// 本地书双轨同步（GAP 170）：关联的书仓文件路径（服务端内部字段——不对外序列化，
    /// 客户端 saveBook 无法改写；仅对账/导入/重扫任务维护）
    #[serde(skip)]
    #[sqlx(rename = "local_file")]
    pub local_file: Option<String>,
    /// 关联文件修改时间（ms epoch——与 local_file_size 一起用于变更检测）
    #[serde(skip)]
    #[sqlx(rename = "local_file_mtime")]
    pub local_file_mtime: i64,
    /// 关联文件大小（字节——与 local_file_mtime 一起用于变更检测）
    #[serde(skip)]
    #[sqlx(rename = "local_file_size")]
    pub local_file_size: i64,
    /// 关联文件删除标记（0=正常；1=文件缺失——书籍/进度/章节保留，文件重现时自动重链，
    /// 避免重复导入产生副本）
    #[serde(skip)]
    #[sqlx(rename = "local_file_deleted")]
    pub local_file_deleted: bool,
    /// 入库行号（list_books 查询附加——前端"最近添加"排序依据；不参与 JSON 写入；
    /// 其他查询无该列时回落 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub rowid: Option<i64>,
}
