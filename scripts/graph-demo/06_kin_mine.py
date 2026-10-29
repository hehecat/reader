#!/usr/bin/env python3
"""v2 专批：亲缘/师承证据挖掘（纯规则 + 正文原文窗口，<=20 字）。

思路：先用人名表（图谱正名 + 非通用别名）在当前句里定位两个名字，
再要求两名字之间（含两端）的连续原文窗口 <=20 字且窗口里出现亲缘/师承字眼，
窗口原文直接当 quote —— 这样每条边都能在正文里逐字查到，不存在臆造。
方向由字眼位置决定：
  甲 + 「长子/次子/之子/儿子/女儿/之弟/之妻/夫人」 + 乙  => 甲是长辈/先者，乙是晚辈/后者
  「其父/生父/父亲/恩师」 + 乙                            => 乙是长辈（甲是句中另一人）
  「女婿」 + 乙 / 甲 + 「岳父」                           => 姻亲，降级 other + label
"""
import json, os, re, sys
from collections import defaultdict
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from gdlib import Chapters, BOOK_JSON, V2

GENERIC = {"陛下", "官家", "大家", "相公", "太尉", "娘子", "殿下", "大王", "将军", "圣人",
           "足下", "老臣", "小底", "官人", "国主", "元帅", "皇嗣", "太皇", "太后", "皇后",
           "太子", "圣人爷", "押班", "大押班", "天子", "康王", "副帅", "老赵", "主公", "首相",
           "中丞", "左监军", "国相", "魏王", "鲁王", "辽王", "申王", "潍王", "濮王", "伪帝",
           "伪太子", "大太子", "三太子", "斡本", "骨卢你太师"}
TITLE = re.compile(r"皇帝|天子|官家|陛下|大家|大王|先生|都统制|元帅|圣人|押班|殿下|相公|"
                   r"太尉|国主|老九|九哥|康王|副帅|圣天子|学士|知府|知州|留守|中丞|"
                   r"舍人|枢相|枢密|参政|尚书|侍郎|御史|经略|都统|统制|统领|防禦|防御|"
                   r"将军|卿|公公|太师|爷爷|爹爹|头领|首领|节度|镇抚|宣抚|资政|龙图|"
                   r"制置|运使|殿帅|大官|押班|妃|娘子|夫人|公子|太子|大王|道君|教主")

# 无向 / 有向候选（(cue, type, 语义方向)）
CUES = [
    # (正则, 类型, 'ea' 表示 cue 前是长辈/前辈、cue 后是晚辈；'eb' 反之)
    (r"(?:的|之)(?:长子|次子|三子|幼子|独子|儿子|大儿子|二儿子|之子|亲儿|亲子)", "parent", "ea"),
    (r"(?:的|之)(?:长女|次女|女儿|闺女|亲女|独女)", "parent", "ea"),
    (r"(?:的|之)(?:弟弟|亲弟|胞弟|二弟|三弟|四弟|幼弟|兄弟|兄长|大哥|二哥|三哥|亲兄|胞兄|妹妹|姐姐|妹子)", "sibling", "ea"),
    (r"(?:的|之)(?:正妻|妻子|发妻|元配|夫人|娘子|妻|妾室|妾|夫君|相公)", "spouse", "ea"),
    (r"(?:的|之)?(?:恩师|师父|老师|座师)", "master", "ea"),
    (r"(?:的|之)(?:弟子|徒弟|门生|学生|高足|传人)", "disciple", "ea"),
    (r"其父|乃父|生父|其母|生母|父亲|家父|老父|爹爹|阿爹|老爹", "parent", "eb"),
    (r"恩师|师父|师从|师事|受业于|从学于|学于", "master", "eb"),
    (r"弟子|徒弟|门生", "disciple", "eb"),
    (r"(?:女婿|女婿儿)", "inlaw_child", "eb"),   # 后面的人 = 女婿（晚辈）
    (r"(?:岳父|岳丈|岳翁)", "inlaw_elder", "eb"),  # 后面的人 = 岳父（长辈）
]


def build_tokens(data):
    tok2 = {}
    for c in data["characters"]:
        tok2.setdefault(c["name"], c["name"])
        for a in c["aliases"]:
            if len(a) < 2 or len(a) > 4 or a in GENERIC or TITLE.search(a):
                continue
            tok2.setdefault(a, c["name"])
    return tok2


def windows(chap, tok2, extra_tokens=()):
    """扫描全书，产出 (type, a, b, quote, chapter, cue) 候选。"""
    toks = sorted(set(tok2) | set(extra_tokens), key=len, reverse=True)
    pat = "|".join(re.escape(t) for t in toks)
    name_re = re.compile(pat)
    cue_res = [(re.compile(c), t, d) for c, t, d in CUES]
    out = []
    for n in chap.ns:
        for s, e, seg in chap.sentences(n):
            names = [(m.start(), m.end(), tok2.get(m.group(), m.group())) for m in name_re.finditer(seg)]
            if not names:
                continue
            for cre, t, d in cue_res:
                for m in cre.finditer(seg):
                    cs, ce = m.start(), m.end()
                    before = [x for x in names if x[1] <= cs + 1 and cs - x[1] <= 6]
                    after = [x for x in names if x[0] >= ce - 1 and x[0] - ce <= 6]
                    if not before or not after:
                        continue
                    x = before[-1]
                    y = after[0]
                    if x[2] == y[2]:
                        continue
                    lo, hi = x[0], y[1]
                    if hi - lo > 20:
                        continue
                    q = seg[lo:hi]
                    if d == "ea":
                        a, b = x[2], y[2]
                    else:
                        a, b = y[2], x[2]        # cue 后的名字是长辈/前辈
                    if t == "inlaw_child":       # 「A女婿B」：B 是 A 的女婿
                        out.append(("other", x[2], y[2], q, n, "女婿"))
                    elif t == "inlaw_elder":     # 「A岳父B」：B 是 A 的岳父
                        out.append(("other", y[2], x[2], q, n, "岳父"))
                    else:
                        out.append((t, a, b, q, n, cre.search(seg).group()))
    return out


def main():
    chap = Chapters()
    data = json.load(open(BOOK_JSON))
    tok2 = build_tokens(data)
    cands = windows(chap, tok2)
    agg = defaultdict(list)
    for t, a, b, q, n, cue in cands:
        agg[(t, a, b)].append((n, q, cue))
    # 同一对子被 NLP 规则在不同字眼下重复命中 -> 取最短窗口，其次最早章
    res = []
    for (t, a, b), hits in agg.items():
        hits.sort(key=lambda h: (len(h[1]), h[0]))
        n, q, cue = hits[0]
        res.append({"type": t, "a": a, "b": b, "chapter": n, "quote": q,
                    "cue": cue, "hits": len(hits)})
    res.sort(key=lambda r: (r["type"], -r["hits"], r["a"]))
    out = os.path.join(V2, "out")
    os.makedirs(out, exist_ok=True)
    json.dump(res, open(os.path.join(out, "kin_mined.json"), "w"), ensure_ascii=False, indent=1)
    from collections import Counter
    print("candidates:", len(res), dict(Counter(r["type"] for r in res)))
    for r in res[:25]:
        print(f"{r['type']:8s} {r['a']}->{r['b']:12s} ch{r['chapter']:<4d} n={r['hits']:<2d} 「{r['quote']}」")


if __name__ == "__main__":
    main()
