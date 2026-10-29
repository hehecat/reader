"""v2 共用工具：加载章节 / 人名表 / 同句共现检索。"""
import json, re, os

CHAP = "/home/hehecat/code/.tmp/gd/chapters.json"
BOOK_JSON = "/home/hehecat/code/reader/frontend/public/graph-demo-data.json"
V2 = "/home/hehecat/code/.tmp/graph-data-v2"

_PUNCT = set("，。、；：？！…—－·「」『』“”‘’（）《》〈〉【】,.;:?!\"'()[]{}<>-~＿ 　/\\|*#@$%^&+=_`")


class Chapters:
    def __init__(self, path=CHAP):
        self.ch = json.load(open(path))
        self.raw = {c["n"]: c["content"] for c in self.ch}
        self.ns = sorted(self.raw)

    def sentences(self, n):
        """切句：按 。！？；…\n 切，附 (start,end) 偏移。"""
        raw = self.raw[n]
        out = []
        for m in re.finditer(r"[^。！？；…\n]{4,}", raw):
            out.append((m.start(), m.end(), m.group()))
        return out

    def find_chapters(self, tok, cap=None):
        hits = [n for n in self.ns if tok in self.raw[n]]
        return hits[:cap] if cap else hits


def sents_with(chap, toks):
    """返回 [(章, start, end, 句子)]，句子同时含 toks 全部元素。"""
    out = []
    for n in chap.ns:
        raw = chap.raw[n]
        if not all(t in raw for t in toks):
            continue
        for s, e, seg in chap.sentences(n):
            if all(t in seg for t in toks):
                out.append((n, s, e, seg))
    return out
