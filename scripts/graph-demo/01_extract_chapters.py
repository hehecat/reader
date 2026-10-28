#!/usr/bin/env python3
"""Step 1: 从生产 SQLite 只读导出目标书籍的全部已缓存章节。

- 只做 SELECT；如需本地操作先 cp 到 .tmp。
- 章节顺序：本库 book_chapters.chapter_index 是雪花 id（无序），
  可靠顺序来自标题里的「第N章」，故以标题数字为准。
用法：
  python3 01_extract_chapters.py [db_path] [out_json]
"""
import json
import re
import sqlite3
import sys

BOOK_URL = "https://m.roushuwu.com/book/50551/"
DB = sys.argv[1] if len(sys.argv) > 1 else "/home/hehecat/code/.tmp/gd/reader.db"
OUT = sys.argv[2] if len(sys.argv) > 2 else "/home/hehecat/code/.tmp/gd/chapters.json"

CH_PAT = re.compile(r"第\s*([0-9零一二三四五六七八九十百千两]+)\s*章")
_CN_DIGIT = {"零": 0, "一": 1, "二": 2, "两": 2, "三": 3, "四": 4,
             "五": 5, "六": 6, "七": 7, "八": 8, "九": 9}
_CN_UNIT = {"十": 10, "百": 100, "千": 1000}


def cn2num(s: str) -> int:
    """中文数字 / 阿拉伯数字 -> int（支持到千位，够用）。"""
    if s.isdigit():
        return int(s)
    total, num = 0, 0
    for ch in s:
        if ch in _CN_DIGIT:
            num = _CN_DIGIT[ch]
        elif ch in _CN_UNIT:
            u = _CN_UNIT[ch]
            total += (num or 1) * u
            num = 0
    return total + num


def main():
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    rows = con.execute(
        "SELECT chapter_index, title, content FROM book_chapters "
        "WHERE book_url=? ORDER BY chapter_index", (BOOK_URL,)).fetchall()
    out, skipped = [], []
    for idx, title, content in rows:
        m = CH_PAT.search(title or "")
        if not m:
            skipped.append(title)          # 卷末总结 / 上架感言 等非正文
            continue
        out.append({"n": cn2num(m.group(1)), "ci": idx,
                    "title": title, "content": content or ""})
    out.sort(key=lambda c: c["n"])

    ns = [c["n"] for c in out]
    assert len(ns) == len(set(ns)), "章号重复"
    missing = [i for i in range(1, max(ns) + 1) if i not in set(ns)]
    json.dump(out, open(OUT, "w"), ensure_ascii=False)
    print(f"rows={len(rows)} numbered={len(out)} skip={len(skipped)}")
    print(f"range=1..{max(ns)} missing={len(missing)} chars={sum(len(c['content']) for c in out)}")
    print("skipped titles:", skipped)


if __name__ == "__main__":
    main()
