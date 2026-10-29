#!/usr/bin/env python3
"""把「敌对关系复核专批」「势力归属专批」的模型结论 + 逐条核过正文的人工校正，
   合成 scripts/graph-demo/v2_overrides.json（03_normalize.py 直接读它）。

三部分来源：
  A. out/review_enemy_*.out.json  149 条原 enemy 边的复核结论（other/enemy/ally）
  B. out/review_faction_01.out.json 13 个 faction=null / 单成员势力 的归属结论
  C. CURATED（本文件里手写，每条 quote 都由 verify_quote 在正文里核过才写入）
任何 quote 核不过正文 -> 不写入 overrides，记入 gaps 里如实报告。
"""
import glob
import importlib.util
import json
import os
import re
import sys
from collections import Counter

V2 = os.environ.get("GD_V2") or "/home/hehecat/code/.tmp/graph-data-v2"
ROOT = "/home/hehecat/code/reader"
sys.path.insert(0, V2)
from gdlib import Chapters            # noqa: E402

spec = importlib.util.spec_from_file_location(
    "norm", os.path.join(ROOT, "scripts/graph-demo/03_normalize.py"))
norm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(norm)

CH = ["n", "content"]
chapters = json.load(open("/home/hehecat/code/.tmp/gd/chapters.json"))
if isinstance(chapters, dict):
    chapters = chapters.get("chapters") or chapters.get("data")
BOOK = norm.Book(chapters)
OUT_JSON = os.path.join(ROOT, "scripts/graph-demo/v2_overrides.json")

gaps, notes = [], []


def vq(quote, chapter, endpoints):
    """正文核验；返回 (chapter, 逐字原文) 或 None"""
    return norm.verify_quote(BOOK, quote, chapter, endpoints)


# ---------------------------------------------------------------- A. 敌对复核
def load_review(pattern):
    res = {}
    for f in sorted(glob.glob(os.path.join(V2, "out", pattern))):
        raw = json.load(open(f))["choices"][0]["message"]["content"]
        txt = re.sub(r"^```(?:json)?|```$", "", raw.strip(), flags=re.M).strip()
        for x in json.loads(txt):
            res[x["id"]] = x
    return res


LABEL_BY_REASON = [
    (r"君臣", "君臣"),
    (r"同僚|同朝|共事|并列", "同僚"),
    (r"上下|部将|部下|主从|节制|统制|帅|将", "上下级"),
    (r"政见|不合|弹劾|训斥|仇怨|争|分歧|看不顺眼|讥", "政见"),
    (r"姻亲|女婿|岳父|亲家", "姻亲"),
    (r"结盟|同盟|联手|结义|追随|效力|归附", "同盟"),
    (r"旧识|故交|仅提及|提及|认识|交往|好友|知交", "旧识"),
]


def label_of(reason, t):
    for pat, lab in LABEL_BY_REASON:
        if re.search(pat, reason or ""):
            return lab
    return "同盟" if t == "ally" else "旧识"


meta = json.load(open(os.path.join(V2, "req/meta_enemy.json")))["items"]
rev = load_review("review_enemy_*.out.json")
retype, n_conf = [], 0
cnt = Counter()
for it in meta:
    x = rev.get(it["id"])
    if not x:
        gaps.append({"kind": "enemy-review-missing", "pair": [it["a"], it["b"]]})
        continue
    new = x["type"]
    if new == "enemy":
        continue
    eps = [it["a"], it["b"]]
    got = vq(x.get("quote"), it["ch"], eps) or vq(it["old_quote"], it["ch"], eps)
    if not got:
        gaps.append({"kind": "quote-unverified", "pair": [it["a"], it["b"]],
                     "quote": x.get("quote")})
        continue
    retype.append({"from": it["a"], "to": it["b"], "old": "enemy", "new": new,
                   "quote": got[1], "chapter": got[0],
                   "label": label_of(x.get("reason"), new),
                   "why": (x.get("reason") or "")[:20]})
    cnt[new] += 1

# ---------------------------------------------------------------- B. 势力归属
FACTION_DECISION = {          # 模型给 low 且正文不支持的，人工改判（见 notes）
    "钟相": None,             # 洞庭湖叛军首领，不属宋廷
    "耶律大石": None,         # 西辽开国者，不属金国
    "合不勒": None,           # 蒙兀部首领，不属金国
    "李清照": "宋廷",
    "范秀娘": "宋廷",         # 范琼之女
}
# 复核确认仍为敌对的：钉住，避免被「度数上限削减」当成弱边砍掉
keep_enemy = [{"from": it["a"], "to": it["b"]}
              for it in meta if (rev.get(it["id"]) or {}).get("type") == "enemy"]

fac_meta = json.load(open(os.path.join(V2, "req/meta_faction.json")))["items"]
frev = load_review("review_faction_*.out.json")
faction = {}
for it in fac_meta:
    x = frev.get(it["id"])
    if x is None:
        gaps.append({"kind": "faction-review-missing", "who": it["name"]})
        continue
    if it["name"] in FACTION_DECISION:
        faction[it["name"]] = FACTION_DECISION[it["name"]]
        continue
    fac = x.get("faction")
    if fac == "其他" or not fac:
        faction[it["name"]] = None
    else:
        faction[it["name"]] = fac

# 方案里钉死的归属（正文证据见 notes）
faction.update({
    "刘晏": "御营",           # 赤心队营将，御营序列
    "郭药师": "金国",         # 常胜军总将，反复北归金国
    "王彦": "八字军",
    "傅选": "八字军",         # 「招安后的八字军统领」
    "张荣": "梁山泊",
    "李逵": "梁山泊",         # 「流浪降服于张荣的李逵李统制」
    "胡闳休": "岳家军",
    "刘正彦": "御营",
    "刘法": "宋廷",
    "岳雷": "岳家军",
    "岳霖": "岳家军",
    "小梁后": "西夏",
    "完颜乌野": "金国",
    "张九成": "宋廷",
    "李侗": "宋廷",
    "程颐": "宋廷",
    "赵佶": "宋廷",
    "吴益": "宋廷",
    "韦太后": "宋廷",
    "郑太后": "宋廷",
})

# ---------------------------------------------------------------- C. 人工校正
ADD_CHARS = [
    {"name": "胡闳休", "aliases": ["胡兄弟"], "gender": "男",
     "role": "岳飞幕府谋主、岳家军某军正将", "faction": "岳家军"},
    {"name": "刘正彦", "aliases": [], "gender": "男",
     "role": "御营右营副统制", "faction": "御营"},
    {"name": "刘法", "aliases": ["刘节度"], "gender": "男",
     "role": "西军旧将、刘正彦之父", "faction": "宋廷"},
    {"name": "岳雷", "aliases": [], "gender": "男", "role": "岳飞次子", "faction": "岳家军"},
    {"name": "岳霖", "aliases": [], "gender": "男", "role": "岳飞三子", "faction": "岳家军"},
    {"name": "小梁后", "aliases": [], "gender": "女", "role": "西夏太后、李乾顺生母",
     "faction": "西夏"},
    {"name": "完颜乌野", "aliases": ["秀才"], "gender": "男",
     "role": "金国宗室、挞懒亲弟", "faction": "金国"},
    {"name": "张九成", "aliases": ["无垢先生", "小张学士"], "gender": "男",
     "role": "太学生、杨时弟子", "faction": "宋廷"},
    {"name": "李侗", "aliases": ["延平先生"], "gender": "男",
     "role": "道学延平先生、杨时罗丛彦弟子", "faction": "宋廷"},
    {"name": "程颐", "aliases": ["伊川先生"], "gender": "男",
     "role": "洛阳道学宗师", "faction": "宋廷"},
    {"name": "赵佶", "aliases": ["道君皇帝", "太上道君皇帝", "太上皇帝"], "gender": "男",
     "role": "太上道君皇帝", "faction": "宋廷"},
    {"name": "吴益", "aliases": [], "gender": "男", "role": "吴近之子、吴贵妃胞弟",
     "faction": "宋廷"},
]

# (from, to, type, quote, chapter, label)
ADD_REL = [
    # --- parent：方向一律 父/母 -> 子/女 ---
    ("完颜阿骨打", "完颜讹里朵", "parent", "阿骨打三子完颜讹里朵", 118, None),
    ("刘法", "刘正彦", "parent", "刘正彦身为刘法之子", 61, None),
    ("岳飞", "岳雷", "parent", "他还有个次子岳雷", 288, None),
    ("岳飞", "岳霖", "parent", "还有个三子岳霖", 288, None),
    ("小梁后", "李乾顺", "parent", "李乾顺生母小梁后", 305, None),
    ("赵佶", "赵玖", "parent", "为父在北国数载", 282, None),
    ("胡安国", "胡宏", "parent", "胡安国次子胡宏", 201, None),
    ("胡安国", "胡寅", "parent", "胡寅的养父兼亲叔叔胡安国", 197, None),
    ("吕好问", "吕本中", "parent", "其父吕好问闻得赵玖言语", 252, None),
    ("吴近", "吴益", "parent", "跪在自己身前的儿子", 253, None),
    ("岳飞", "岳云", "parent", "长子却已经十三了", 288, None),
    ("完颜娄室", "完颜谋衍", "parent", "二十来岁的次子", 216, None),
    # --- sibling ---
    ("完颜阿骨打", "完颜吴乞买", "sibling", "这是阿骨打之弟", 10, None),
    ("完颜活女", "完颜谋衍", "sibling", "完颜活女之弟", 216, None),
    ("完颜挞懒", "完颜乌野", "sibling", "完颜乌野乃是挞懒的亲弟弟", 271, None),
    ("胡寅", "胡宏", "sibling", "胡寅的堂弟兼养弟胡宏", 197, None),
    # --- spouse ---
    ("赵佶", "郑太后", "spouse", "郑太后是太上道君皇帝的正宫", 281, None),
    # --- master / disciple（每对师承双向编码，图谱两个方向都能走）---
    ("周同", "岳飞", "master", "恩师周同那里学骑射武艺", 11, None),
    ("岳飞", "周同", "disciple", "恩师周同那里学骑射武艺", 11, None),
    ("周同", "张显", "master", "恩师周同那里学骑射武艺", 11, None),
    ("张显", "周同", "disciple", "恩师周同那里学骑射武艺", 11, None),
    ("周同", "汤怀", "master", "恩师周同那里学骑射武艺", 11, None),
    ("汤怀", "周同", "disciple", "恩师周同那里学骑射武艺", 11, None),
    ("周同", "王贵", "master", "恩师周同那里学骑射武艺", 11, None),
    ("王贵", "周同", "disciple", "恩师周同那里学骑射武艺", 11, None),
    ("宗泽", "郦琼", "master", "郦琼天然就可以自称是宗泽的弟子", 150, None),
    ("郦琼", "宗泽", "disciple", "郦琼天然就可以自称是宗泽的弟子", 150, None),
    ("杨时", "罗丛彦", "master", "此人是龟山先生（杨时）弟子", 258, None),
    ("罗丛彦", "杨时", "disciple", "此人是龟山先生（杨时）弟子", 258, None),
    ("杨时", "张九成", "master", "正是道学宗师杨时的弟子", 358, None),
    ("张九成", "杨时", "disciple", "正是道学宗师杨时的弟子", 358, None),
    ("杨时", "李侗", "master", "算是龟山先生子弟", 258, None),
    ("李侗", "杨时", "disciple", "算是龟山先生子弟", 258, None),
    ("罗丛彦", "李侗", "master", "也算是豫章先生子弟", 258, None),
    ("李侗", "罗丛彦", "disciple", "也算是豫章先生子弟", 258, None),
    ("程颐", "马伸", "master", "程颐嫡传弟子", 55, None),
    ("马伸", "程颐", "disciple", "程颐嫡传弟子", 55, None),
    # --- enemy：原抽取没抽到的真实交战（宋金/伪齐战场，正文短句可查）---
    ("韩世忠", "耶律余睹", "enemy", "若非韩世忠到底是击退了耶律余睹", 113, None),
    ("韩世忠", "完颜活女", "enemy", "完颜活女夹击韩世忠", 226, None),
    ("张宪", "李成", "enemy", "被张宪大股杀伤的李成部步卒", 286, None),
    ("吴玠", "突合速", "enemy", "射穿了突合速的脚", 376, None),
    ("李成", "李逵", "enemy", "李成击败，最后流浪降服于张荣的李逵", 123, None),
    # --- other：新人物的落点（都是正文可查的同僚/上下级）---
    ("岳飞", "胡闳休", "other", "胡闳休则是岳飞的总参谋长", 105, "幕僚"),
    ("辛永宗", "胡闳休", "other", "胡兄弟看在咱们一起颠沛流离一年的份上", 105, "同僚"),
    ("王渊", "刘正彦", "other", "右营副统制刘正彦正在营中", 12, "上下级"),
    ("赵玖", "赵佶", "other", "二位太上皇帝一路辛苦", 282, "太上皇"),
]

add_chars = {c["name"] for c in ADD_CHARS}
known = {c["name"] for c in json.load(
    open(os.path.join(ROOT, "frontend/public/graph-demo-data.json")))["characters"]}
known |= add_chars

add_rel, seen_pairs = [], set()
for a, b, t, q, ch, lab in ADD_REL:
    if a not in known or b not in known:
        gaps.append({"kind": "endpoint-not-in-graph", "pair": [a, b]})
        continue
    got = vq(q, ch, [a, b])
    if not got:
        gaps.append({"kind": "quote-unverified", "pair": [a, b], "quote": q})
        continue
    k = (frozenset((a, b)), t) if t in ("sibling", "spouse", "ally", "enemy") else (a, b, t)
    if k in seen_pairs:
        continue
    seen_pairs.add(k)
    add_rel.append({"from": a, "to": b, "type": t, "quote": got[1],
                    "chapter": got[0], "label": lab})

FACTION_LEADER = {"御营": "王渊", "八字军": "王彦", "梁山泊": "张荣"}   # 正文点明的长官
over = {"retype": retype, "keep_enemy": keep_enemy, "faction": faction,
        "faction_leader": FACTION_LEADER,
        "add_characters": ADD_CHARS, "add_relations": add_rel,
        "drop_relations": [], "gaps": gaps, "notes": notes}
json.dump(over, open(OUT_JSON, "w"), ensure_ascii=False, indent=1)

print("retype:", len(retype), dict(cnt), "keep_enemy:", len(keep_enemy))
print("faction:", json.dumps(faction, ensure_ascii=False))
print("add_characters:", len(ADD_CHARS), "add_relations:", len(add_rel))
print("gaps:", len(gaps))
for g in gaps:
    print("  gap:", json.dumps(g, ensure_ascii=False))
print("->", OUT_JSON)
