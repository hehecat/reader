//! 书籍核心 API：书架、详情、目录、正文、阅读进度、换源、分组、书签、本地书（TXT/EPUB）导入与预览

mod chapter;
mod groups_bookmarks;
mod helpers;
mod local;
mod shelf;
#[cfg(test)]
mod tests;

pub use chapter::{get_book_content, get_chapter_list, get_chapter_list_by_rule};
pub use groups_bookmarks::{
    delete_book_group, delete_bookmark, delete_bookmarks, get_book_groups, get_bookmarks,
    save_book_group, save_book_group_id, save_bookmark,
};
pub(crate) use helpers::{resolve_loc_book_file, resolve_local_file_path, txt_toc_rule_regexes};
pub use local::{import_book_preview, upload_local_book};
pub use shelf::{
    delete_book, delete_books, get_book_info, get_bookshelf, save_book, save_book_progress,
    set_book_source,
};
