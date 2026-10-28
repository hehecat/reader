#!/usr/bin/env python3
"""Step 3: 多批抽取结果归一化 -> graph-demo-data.json。

输入：
  .tmp/gd/batches/llm_*.json      8 批 LLM 原始响应（每批一次独立抽取）
  .tmp/gd/chapters.json           395 章正文（用于出处校验 / 出场区间）
输出：
  frontend/public/graph-demo-data.json
  scripts/graph-demo/llm-batches.json   （合并后的原始抽取，便于离线复跑）

相比 v1（单批 80 章）的主要改动：
  * 跨批按别名并查集合并人物（v1 只有单批，没有跨批合并）；
  * **出处校验**：每条关系的 quote 回到正文里逐章找，找到就地把 from_chapter 改成
    真实出现的那一章，并把 quote 修正为逐字原文（<=20 字）；找不到出处的边直接丢弃
    —— 这是「不许臆造关系」的硬约束；
  * generation 用「配偶/兄弟同代 + 父母差 1」的约束图上算，父子代差必须 =1，
    矛盾的 parent 边（代差 >1 / 成环 / 父母与子女同组）会被丢弃并统计；
  * 势力名归一（宋廷/金国/伪齐/西夏/赤心队/岳家军/常胜军/御营/梁山泊/八字军），
    并允许「role 里点名的势力」补 faction（如 role=赤心队首领 -> faction=赤心队）；
  * 孤立点（degree=0）全部剔除；中心度比值打表。
"""
import json
import os
import re
from collections import Counter, defaultdict, deque

BOOK = "绍宋"
SCHEMA = "graph-demo-v1"
TOTAL_CHAPTERS = 395     # 已缓存且有正文章号的章节数
GAP = 20                 # 「>=20 章未出场」视为角色线结束
OUT = "/home/hehecat/code/reader/frontend/public/graph-demo-data.json"
BATCH_DIR = "/home/hehecat/code/.tmp/gd/batches"
CHAP = "/home/hehecat/code/.tmp/gd/chapters.json"
META = "/home/hehecat/code/.tmp/gd/sample_meta.json"
SAVED = os.path.join(os.path.dirname(os.path.abspath(__file__)), "llm-batches.json")

GENERIC = {"陛下", "官家", "大家", "相公", "太尉", "娘子", "殿下", "大王",
           "将军", "圣人", "足下", "老臣", "小底", "官人", "国主", "元帅",
           "皇嗣", "官家爷", "太皇", "太后", "皇后", "太子", "圣人爷",
           "押班", "大押班", "大臣", "群臣", "诸将"}
BAD_ROLE = {"主角", "配角", "反派", "路人", "龙套", "次要人物", "人物", "武将", "文臣",
            "未知", "未知人物", "", "配角（仅被提及）", "群像", "众人"}

LABEL = {"parent": "亲缘", "spouse": "夫妻", "sibling": "手足", "master": "师徒",
         "disciple": "师徒", "ally": "同盟", "enemy": "敌对", "other": "其他"}
SYMMETRIC = {"spouse", "sibling", "ally", "enemy"}
TYPES = set(LABEL)

# 势力名归一：原始名 -> 标准名
FACTION_MAP = {
    "宋廷": "宋廷", "大宋": "宋廷", "宋朝": "宋廷", "南宋": "宋廷", "赵宋": "宋廷",
    "朝廷": "宋廷", "东京留守司": "宋廷", "相州集团": "宋廷", "延福宫": "宋廷",
    "宋": "宋廷", "东京朝廷": "宋廷", "御前": "宋廷",
    "金国": "金国", "金军": "金国", "金廷": "金国", "大金": "金国", "女真": "金国",
    "伪齐": "伪齐", "齐": "伪齐", "刘齐": "伪齐", "齐国": "伪齐",
    "西夏": "西夏", "夏国": "西夏",
    "赤心队": "赤心队",
    "岳家军": "岳家军", "岳部": "岳家军", "岳家军部": "岳家军",
    "常胜军": "常胜军",
    "御营": "御营", "御营军": "御营", "御营司": "御营",
    "梁山泊": "梁山泊", "梁山": "梁山泊",
    "八字军": "八字军",
}
FACTION_KIND = {"宋廷": "势力", "金国": "势力", "伪齐": "势力", "西夏": "势力",
                "御营": "组织", "赤心队": "组织", "岳家军": "组织",
                "常胜军": "组织", "梁山泊": "组织", "八字军": "组织"}
MAX_FACTIONS = 10
# ---- demo 规模收敛（父 agent 要求：人物 120-140 / 关系 240-300，最大度数 <12%）----
TARGET_CHARS = 133          # 保留人物基准（亲缘/关键人物可顶到 MAX_CHARS）
MAX_CHARS = 139             # 硬上限
TARGET_RELS_MAX = 300       # 关系上限
TARGET_RELS_MIN = 240       # 关系下限（砍边时到这儿就停）
CAP_FRAC = 0.115            # 单点度数上限
CAP_REF = 265               # 上限的参照关系数（避免「砍边→上限跟着降」的死循环）
EDGE_RANK = {"parent": 0, "spouse": 0, "sibling": 0, "master": 0, "disciple": 0,
             "enemy": 1, "ally": 2, "other": 3}
# 势力：role 里点名的「役属势力」也算 membership（岳飞部将 -> 岳家军）
ROLE_FACTION_CUE = [
    ("赤心队", r"赤心队"),
    ("常胜军", r"常胜军"),
    ("岳家军", r"岳飞部|岳飞长子|岳飞次子|岳飞之弟|岳飞帐|岳飞麾|岳家军|岳飞亲兵"),
    ("八字军", r"八字军"),
    ("御营", r"御营"),
    ("西夏", r"西夏"),
    ("伪齐", r"伪齐"),
    ("金国", r"金国|金军"),
]
# 正文里存在、但模型没给 leader 的小势力：直接钉一个领袖，保证势力不消失
SEED_FACTIONS = [("赤心队", "刘晏"), ("常胜军", "郭药师")]

# 名字相近但不是同一人的别名（否则会把两个人的边并到一个人头上）
ALIAS_DROP = {"胡寅": {"胡闳休"}, "刘光世": {"刘正彦"}, "王彦": {"王彦章"}}

# 逐条对过正文的手工校正（模型把方向/端点/类型搞错的亲缘边）
# 键都是「无向对」：存成 a->b 还是 b->a 都能命中
KIN_FLIP = {                     # 值 = 正文支持的正确方向 (父/母, 子/女)
    ("完颜兀术", "完颜阿骨打"): ("完颜阿骨打", "完颜兀术"),   # 「咱爹爹」= 兀术说阿骨打
    ("刘子羽", "刘韐"): ("刘韐", "刘子羽"),                  # 「其父刘韐」
    ("赵玖", "韦太后"): ("韦太后", "赵玖"),                  # 「官家的'生母'韦太后」
}
KIN_RETYPE = {                   # 键 (a, b, 旧类型)；值 (新类型, label)
    ("赵玖", "郑太后", "parent"): ("other", "嫡母"),          # 郑是太上道君正宫，非赵玖生母
    ("完颜挞懒", "蒲察鹘拔鲁", "parent"): ("other", "女婿"),   # 「女婿、也就是蒲察鹘拔鲁」
    ("薛元礼", "高守义", "parent"): ("other", "岳父"),         # 「他岳父薛元礼」
    ("李光", "李经", "sibling"): ("ally", "至交"),            # 李经是李纲三弟，李光与李纲是至交
}
KIN_LABEL = {                    # 结义兄弟：保留 sibling（留在家族树），用 label 说明是义兄弟
    ("张显", "岳飞", "sibling"): "结义兄弟",
    ("汤怀", "岳飞", "sibling"): "结义兄弟",
    ("王贵", "岳飞", "sibling"): "结义兄弟",
}
EDGE_DROP_KIN = {("汪伯彦", "胡寅", "parent")}                              # 正文是女婿胡闳休，不是胡寅

# 同一人的汉字异写（正文两种写法都出现）：合并到规范名
FORCE_MERGE = {"完颜拔里速": "完颜拔离速"}

# 手工校正涉及的人（校正后他们的边变成 other，按分数会被裁掉，必须钉住）
KEEP_CHARS = {"郑太后", "韦太后", "刘韐", "刘子羽", "蒲察鹘拔鲁", "完颜挞懒",
              "薛元礼", "高守义", "李光", "李经", "张显", "汤怀", "王贵", "岳飞"}

# 常见称谓补全（写入前用正文核验，只补能查到出处的）
EXTRA_ALIASES = {
    "赵玖": ["官家", "大家", "陛下", "赵老九", "九哥", "赵官家", "赵九"],
    "岳飞": ["岳鹏举", "鹏举", "岳太尉"],
    "韩世忠": ["韩良臣", "良臣", "韩太尉"],
    "宗泽": ["宗汝霖", "汝霖", "宗留守"],
    "完颜兀术": ["金兀术", "兀术", "四太子"],
    "张浚": ["张德远", "德远"],
    "赵鼎": ["赵元镇", "元镇"],
    "秦桧": ["秦会之", "会之"],
    "李纲": ["李伯纪", "伯纪"],
}
LEADER_WORD = re.compile(r"首领|总将|主帅|都统制|都统|皇帝|国主|大王|王爷|宰相|丞相|"
                         r"枢密使|留守|节度|大元帅|元帥|统帅|统领")
# other 类边只保留 quote 里带社会关系线索的（纯「头衔共现」不算关系）
REL_QUOTE_REPAIR = True
KEEP_PARENT_CUE = True
PARENT_CUE = re.compile(r"之子|长子|次子|幼子|其子|儿子|皇子|公主|爹爹|老爹|爹|其父|生父|"
                        r"父亲|家父|亲女|女儿|岳父|女婿|所出|正宫|生母|乳名|亲弟|亲子")
SPOUSE_ANY = re.compile(r"妻|夫人|娘子|娶|嫁|成亲|为后|贵妃|正宫|妃子|大婚|夫君")
SIBLING_ANY = re.compile(r"兄长|弟弟|哥哥|兄弟|胞弟|亲弟|族兄|堂兄|义弟|结义|二弟|三弟|四弟|小弟|阿弟")
DISCIPLE_ANY = re.compile(r"弟子|徒弟|门生|拜师|受业|学于|从学|恩师|师父")
FIX_MASTER = True
MASTER_CUE = re.compile(r"恩师|师父|师从|从学于|门下|授业|弟子|徒弟|门生|拜师|受业|学于|师事|传其衣钵")
KEEP_ALLY_CUE = True
DROP_CONFLICT = True
ALLY_CUE = re.compile(
    r"心腹|亲信|同盟|结盟|联手|联合|相约|一党|同党|合作|共谋|倚重|亲厚|相善|交好|盟|"
    r"归附|归顺|投效|效力|效死|听命|唯命|追随|拥戴|辅佐|同心|倚为|亲兵|部将|部下|麾下|"
    r"生死之交|结义|义兄弟")
KEEP_OTHER_CUE = True
OTHER_CUE = re.compile(
    r"主公|部将|部下|麾下|属下|心腹|亲信|幕僚|军师|举荐|推荐|引荐|收留|随侍|投奔|来降|"
    r"投降|召见|召来|遣使|委派|拜见|拜为|拜入|授|封为|门生|学生|同学|同僚|同党|一党|"
    r"翁婿|女婿|姻亲|结盟|相约|联手|联合|弹劾|参劾|请和|降|仇|怨|庇护|护送|教导|训练|"
    r"督战|节制|统辖|隶属|归附|归顺|投效|从军|麾|请降|议和|使节|使者|学生|受业|"
    r"父子|之弟|之兄|之妻|之子|侄|甥|妻|婿|族|宗|师")

PUNCT = set("，。、；：？！…—－·「」『』“”‘’（）《》〈〉【】,.;:?!\"'()[]{}<>-~＿ 　/\\|*#@$%^&+=_`"
            "0123456789０１２３４５６７８９")


def norm_text(text):
    """去空白/标点，返回 (normalized, idx) —— idx[i] 是 normalized[i] 在原文里的下标。"""
    buf, idx = [], []
    for i, ch in enumerate(text):
        if ch.isspace() or ch in PUNCT:
            continue
        buf.append(ch)
        idx.append(i)
    return "".join(buf), idx


# ---------------------------------------------------------------- 读入
def load_batches():
    raws = []
    if os.path.isdir(BATCH_DIR):
        files = sorted(f for f in os.listdir(BATCH_DIR) if re.fullmatch(r"llm_\d+\.json", f))
        for f in files:
            resp = json.load(open(os.path.join(BATCH_DIR, f)))
            raws.append(resp["choices"][0]["message"]["content"])
    if not raws and os.path.exists(SAVED):
        return json.load(open(SAVED))["batches"]
    batches = []
    for raw in raws:
        raw = re.sub(r"^```(?:json)?|```$", "", raw.strip(), flags=re.M).strip()
        try:
            batches.append(json.loads(raw))
        except Exception as exc:                        # 单批坏了不影响其它批
            print(f"[warn] 跳过无法解析的一批: {exc}")
    if raws:
        json.dump({"batches": batches}, open(SAVED, "w"), ensure_ascii=False)
    return batches


PART = re.compile(r"^(.{2,4})部(.{2,4})$")


_CL_TOKS = []


def toks_of(i):
    return _CL_TOKS[i]


def prune_for_demo(clusters, rels, dropped, must_keep=()):
    """把图裁到 demo 可读的规模：留关键人物、压单点度数、限关系总数。"""
    deg, kin, en = Counter(), Counter(), Counter()
    for r in rels:
        deg[r["a"]] += 1
        deg[r["b"]] += 1
        if r["t"] in ("parent", "spouse", "sibling", "master", "disciple"):
            kin[r["a"]] += 1
            kin[r["b"]] += 1
        elif r["t"] == "enemy":
            en[r["a"]] += 1
            en[r["b"]] += 1
    span = {i: max(1, clusters[i]["lastSeen"] - clusters[i]["firstSeen"])
            for i in range(len(clusters))}
    # 打分：有亲缘 >> 有敌对 >> 关系多 >> 出场跨度长
    score = {i: 3.0 * kin[i] + 1.5 * en[i] + 1.0 * deg[i] + 0.03 * span[i]
             for i in range(len(clusters))}
    cand = [i for i in range(len(clusters)) if deg[i] > 0]
    cand.sort(key=lambda i: (-score[i], clusters[i]["firstSeen"], i))
    keep = cand[:TARGET_CHARS]
    must = set(must_keep)
    # 亲缘/师承边上的人一律保住（「家族树」视图要有内容），再从低分非亲缘人物里挤出名额
    kin_chars = set()
    for r in rels:
        if EDGE_RANK[r["t"]] == 0:
            kin_chars.add(r["a"])
            kin_chars.add(r["b"])
    for i in sorted((kin_chars | must) - set(keep), key=lambda i: (-score[i], i)):
        if deg[i] > 0:
            keep.append(i)
    if len(keep) > MAX_CHARS:
        spare = sorted((i for i in keep if i not in kin_chars and i not in must),
                       key=lambda i: (score[i], -clusters[i]["firstSeen"]))
        while len(keep) > MAX_CHARS and spare:
            keep.remove(spare.pop(0))
    keep.sort()
    dropped["超规模截断人物"] = len(cand) - len(keep)
    ks = set(keep)
    before = len(rels)
    rels = [r for r in rels if r["a"] in ks and r["b"] in ks]
    dropped["截断人物带掉的关系"] = before - len(rels)

    def rank(r):                              # 越小越该保留
        if r.get("pin"):
            return (-1, -len(r["q"]), r["ch"])
        return (EDGE_RANK[r["t"]], -len(r["q"]), r["ch"])

    cap = int(CAP_FRAC * CAP_REF)
    while len(rels) > TARGET_RELS_MIN:
        d = Counter()
        for r in rels:
            d[r["a"]] += 1
            d[r["b"]] += 1
        over = [i for i, v in d.items() if v > cap]
        if not over:
            break
        drop = set()
        for i in over:                        # 每个超限节点砍掉最弱的一条
            mine = sorted((r for r in rels if i in (r["a"], r["b"])),
                          key=rank, reverse=True)
            if mine:
                drop.add(id(mine[0]))
                dropped["度数上限削减"] += 1
        rels = [r for r in rels if id(r) not in drop]

    if len(rels) > TARGET_RELS_MAX:           # 关系总数封顶，先动 other/ally
        drop = set()
        for r in sorted(rels, key=rank, reverse=True):
            if len(rels) - len(drop) <= TARGET_RELS_MAX:
                break
            if EDGE_RANK[r["t"]] == 0:
                continue
            drop.add(id(r))
        rels = [r for r in rels if id(r) not in drop]
        dropped["关系总数削减"] = len(drop)

    d = Counter()
    for r in rels:
        d[r["a"]] += 1
        d[r["b"]] += 1
    keep2 = [i for i in keep if d[i] > 0]
    dropped["孤立点"] += len(keep) - len(keep2)
    remap = {old: new for new, old in enumerate(keep2)}
    rels = [{"a": remap[r["a"]], "b": remap[r["b"]], "t": r["t"], "ch": r["ch"], "q": r["q"],
             **({"label": r["label"]} if r.get("label") else {})}
            for r in rels]
    return [clusters[i] for i in keep2], rels, remap


def pair_key(r):
    a, b = r["a"], r["b"]
    return (min(a, b), max(a, b))


def other_cue_hit(r):
    return bool(ALLY_CUE.search(r["q"]))


def both_named(r):
    q = r["q"]
    return all(any(n in q for n in toks_of(r[k])) for k in ("a", "b"))


def clean_name(s):
    s = (s or "").strip().strip("\"'“”‘’「」『』")
    if not s or len(s) < 2 or s in GENERIC:
        return ""
    if not re.fullmatch(r"[\u4e00-\u9fff]{2,6}", s):
        return ""
    return s


def split_name(s):
    """「张荣部尤学究」这种拼接名 -> (尤学究, 原名留作别名)。"""
    m = PART.match(s)
    if m and m.group(2) not in GENERIC:
        return m.group(2), s
    if s.endswith("部") and len(s) >= 3:
        return s[:-1], s
    return s, None


# ---------------------------------------------------------------- 人物合并
def merge_characters(batches):
    """跨批并查集合并：名字相等 / 一方名字是另一方别名 / 共享 >=3 字别名。"""
    recs = []
    for b in batches:
        for c in b.get("characters") or []:
            nm = clean_name(c.get("name"))
            if not nm:
                continue
            nm, extra = split_name(nm)
            toks = {nm}
            if extra:
                toks.add(extra)
            for a in c.get("aliases") or []:
                a = clean_name(a)
                if a and a != nm:
                    toks.add(a)
            fac = c.get("faction")
            fac = FACTION_MAP.get((fac or "").strip()) if fac else None
            recs.append({"name": nm, "tokens": toks,
                         "gender": (c.get("gender") or "未知").strip(),
                         "role": (c.get("role") or "").strip(),
                         "faction": fac})

    parent = list(range(len(recs)))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[max(ra, rb)] = min(ra, rb)

    tok2idx = defaultdict(list)
    for i, r in enumerate(recs):
        for t in r["tokens"]:
            tok2idx[t].append(i)
    for i, r in enumerate(recs):
        for t in r["tokens"]:
            for j in tok2idx[t]:
                if j <= i:
                    continue
                other = recs[j]
                # 只按「名字」合并，不用别名互相重合（别名表是模型给的，噪音大）
                if (r["name"] == other["name"] or r["name"] in other["tokens"]
                        or other["name"] in r["tokens"]):
                    union(i, j)

    # 强制合并：正文里同一人的两种汉字写法（模型按两种写法各建了一个人）
    name2idx = {}
    for i, r in enumerate(recs):
        name2idx.setdefault(r["name"], i)
    for variant, canonical in FORCE_MERGE.items():
        i, j = name2idx.get(variant), name2idx.get(canonical)
        if i is not None and j is not None:
            union(i, j)

    groups = defaultdict(list)
    for i in range(len(recs)):
        groups[find(i)].append(i)

    clusters = []
    for _, idxs in sorted(groups.items()):
        name_cnt = Counter(recs[i]["name"] for i in idxs)
        canonical = sorted(name_cnt.items(), key=lambda kv: (-kv[1], -len(kv[0])))[0][0]
        toks = set()
        for i in idxs:
            toks |= recs[i]["tokens"]
        toks.discard(canonical)
        role_cnt = Counter(recs[i]["role"] for i in idxs
                           if recs[i]["role"] and recs[i]["role"] not in BAD_ROLE)
        role = sorted(role_cnt.items(), key=lambda kv: (-kv[1], -len(kv[0])))[0][0] if role_cnt else ""
        fac_cnt = Counter(recs[i]["faction"] for i in idxs if recs[i]["faction"])
        faction = sorted(fac_cnt.items(), key=lambda kv: -kv[1])[0][0] if fac_cnt else None
        gender = Counter(recs[i]["gender"] for i in idxs).most_common(1)[0][0]
        clusters.append({"name": canonical, "aliases": sorted(toks), "role": role,
                         "faction": faction, "gender": gender,
                         "records": len(idxs)})

    # 强制合并后统一正名（异写名降为别名）
    for c in clusters:
        tgt = FORCE_MERGE.get(c["name"])
        if tgt:
            c["aliases"] = sorted(set(c["aliases"]) | {c["name"]})
            c["name"] = tgt

    # 别名卫生：若某别名正是另一个 cluster 的名字，说明是模型串了人，剔除
    all_names = {c["name"] for c in clusters}
    for c in clusters:
        c["aliases"] = [a for a in c["aliases"] if a not in all_names]

    # 共享别名的冲突：同一别名被两个 cluster 同时认领时，从两边都剔除（只留名字）
    owner = defaultdict(set)
    for k, c in enumerate(clusters):
        for t in [c["name"]] + c["aliases"]:
            owner[t].add(k)
    ambiguous = {t for t, s in owner.items() if len(s) > 1}
    canon = {c["name"] for c in clusters}
    for c in clusters:
        c["aliases"] = [a for a in c["aliases"]
                        if a not in ambiguous and a != c["name"]
                        and a not in canon]        # 别名是另一个人的正名 -> 不是同一个人
    return clusters, ambiguous


# ---------------------------------------------------------------- 正文索引
class Book:
    def __init__(self, chapters):
        self.norm, self.idx, self.raw = {}, {}, {}
        for c in chapters:
            nrm, idx = norm_text(c["content"])
            self.norm[c["n"]] = nrm
            self.idx[c["n"]] = idx
            self.raw[c["n"]] = c["content"]
        self.ns = sorted(self.norm)

    def find(self, nq):
        return [n for n in self.ns if nq in self.norm[n]]

    def slice(self, n, pos, length, cap=20):
        """把 (章, normalized 起点, 长度) 还原成逐字原文，最多 cap 字。"""
        idx = self.idx[n]
        if pos + length > len(idx):
            length = len(idx) - pos
        if length <= 0:
            return None
        start = idx[pos]
        end = idx[pos + length - 1] + 1
        s = self.raw[n][start:end]
        if len(s) > cap:                       # 归一化丢了标点，原文可能更长
            s = s[:cap].rstrip()
        return s if len(s) >= 4 else None


def verify_quote(book, quote, claimed, endpoints):
    """在正文里核验 quote；返回 (chapter, quote_verbatim) 或 None。

    顺序：先用模型给的章号 -> 全书找整句 -> 按标点切段找 >=5 字段。
    命中多个章时优先「两个端点名字都出现」的章，其次最早。
    """
    nq, _ = norm_text(quote or "")
    if len(nq) < 4:
        return None

    def pick(hits, needle):
        if not hits:
            return None

        def score(n):
            txt = book.raw[n]
            return (-sum(1 for e in endpoints if e and e in txt), n)

        best = sorted(hits, key=score)[0]
        pos = book.norm[best].find(needle)
        q = book.slice(best, pos, len(needle))
        return (best, q) if q else None

    if isinstance(claimed, int) and claimed in book.norm:
        pos = book.norm[claimed].find(nq)
        if pos >= 0:
            q = book.slice(claimed, pos, len(nq))
            if q:
                return claimed, q
    got = pick(book.find(nq), nq)
    if got:
        return got
    for seg in sorted(re.split(r"[^\u4e00-\u9fff]+", quote or ""), key=len, reverse=True):
        ns, _ = norm_text(seg)
        if len(ns) < 5:
            continue
        got = pick(book.find(ns), ns)
        if got:
            return got
    return None


# ---------------------------------------------------------------- 亲缘纠偏
def repair_by_quote(a, b, t, quote):
    """用模型自己给的原文引用做一次方向/类型体检（规则化，不看具体人名）。"""
    if not quote:
        return a, b, t
    if t == "parent":
        elder = re.search(r"的(?:爹|父|父亲|阿爹)|之父|其父|乃父", quote)
        child = re.search(r"长子|次子|幼子|儿子|之子|女儿|独子|亲生|的亲儿", quote)
        if elder and not child:
            a, b = b, a
    if t in ("parent", "sibling", "spouse") and re.search(
            r"侄子|侄儿|叔父|伯父|外甥|堂兄|表兄|侄女|外孙女", quote):
        t = "other"
    return a, b, t


# ---------------------------------------------------------------- 主体
def main():
    batches = load_batches()
    chapters = json.load(open(CHAP))
    book = Book(chapters)
    clusters, ambiguous = merge_characters(batches)
    _CL_TOKS[:] = [[c["name"]] + c["aliases"] for c in clusters]

    tok2c, owner = {}, defaultdict(set)
    for k, c in enumerate(clusters):
        for t in [c["name"]] + c["aliases"]:
            owner[t].add(k)
    for t, s in owner.items():
        if len(s) == 1:
            tok2c[t] = next(iter(s))
    long_toks = sorted([t for t in tok2c if len(t) >= 3], key=len, reverse=True)

    def resolve(name):
        nm = (name or "").strip().strip("\"'“”‘’「」")
        if nm in tok2c:
            return tok2c[nm]
        for t in long_toks:                      # 「岳飞长子岳云」这类长名退化到已知人名
            if t in nm:
                return tok2c[t]
        nm2 = clean_name(nm)
        if nm2 in tok2c:
            return tok2c[nm2]
        return None

    # --- 关系：解析端点 + 类型纠偏 + 出处校验 ---
    raw_rels, dropped = [], Counter()
    for b in batches:
        for r in b.get("relations") or []:
            a, c = resolve(r.get("from")), resolve(r.get("to"))
            if a is None or c is None:
                dropped["端点未知"] += 1
                continue
            t = (r.get("type") or "").strip()
            if t not in TYPES:
                t = "other"
            quote = (r.get("quote") or "").strip().strip("\"'“”「」")
            a, c, t = repair_by_quote(a, c, t, quote)
            if a == c:
                dropped["自环"] += 1
                continue
            eps = [clusters[a]["name"]] + clusters[a]["aliases"] + \
                  [clusters[c]["name"]] + clusters[c]["aliases"]
            got = verify_quote(book, quote, r.get("chapter"), eps)
            if not got:
                dropped["出处核验失败"] += 1
                continue
            ch, qv = got
            raw_rels.append({"a": a, "b": c, "t": t, "ch": ch, "q": qv})

    # --- 亲缘出处修复：quote 里没有亲缘线索时，回正文找一句同时套住两端的话 ---
    KIN_CUE_BY_TYPE = {"parent": PARENT_CUE, "master": MASTER_CUE,
                       "spouse": SPOUSE_ANY, "sibling": SIBLING_ANY,
                       "disciple": DISCIPLE_ANY}
    if REL_QUOTE_REPAIR:
        where = {}
        for i, c in enumerate(clusters):
            toks = [t for t in [c["name"]] + c["aliases"] if len(t) >= 2]
            where[i] = [n for n in book.ns if any(t in book.raw[n] for t in toks)]

        def find_kin(a, c, cue):
            for n in sorted(set(where.get(a, [])) & set(where.get(c, []))):
                raw = book.raw[n]
                pa, pc = [], []
                for t in [clusters[a]["name"]] + clusters[a]["aliases"]:
                    if len(t) >= 2:
                        pa += [m.start() for m in re.finditer(re.escape(t), raw)]
                for t in [clusters[c]["name"]] + clusters[c]["aliases"]:
                    if len(t) >= 2:
                        pc += [m.start() for m in re.finditer(re.escape(t), raw)]
                for x in pa:
                    for y in pc:
                        lo, hi = min(x, y), max(x, y) + 2
                        if hi - lo > 20:
                            continue
                        snippet = raw[lo:hi]
                        if "\n" in snippet or "\r" in snippet:   # 跨段落拼出来的不是原句
                            continue
                        if cue.search(snippet) or cue.search(raw[max(0, lo - 12):hi + 12]):
                            return n, snippet.strip("\"'“”‘’「」，。、；： ")
            return None

        fixed, kept = 0, []
        for r in raw_rels:
            cue = KIN_CUE_BY_TYPE.get(r["t"])
            if cue is None or cue.search(r["q"]):
                kept.append(r)
                continue
            hit = find_kin(r["a"], r["b"], cue)
            if hit:
                r["ch"], r["q"] = hit[0], hit[1]
                fixed += 1
                kept.append(r)
            else:
                dropped["亲缘-无出处"] += 1
        raw_rels = kept
        dropped["亲缘出处修复"] = fixed

    # --- 去重：同类型同端点只留 quote 最长（其次最早章）的一条 ---
    best = {}
    for r in raw_rels:
        key = (min(r["a"], r["b"]), max(r["a"], r["b"]), r["t"]) if r["t"] in SYMMETRIC \
            else (r["a"], r["b"], r["t"])
        old = best.get(key)
        if old is None or (len(r["q"]), -r["ch"]) > (len(old["q"]), -old["ch"]):
            best[key] = r
    # 有向边双向冲突（parent/master 同时存在 A->B 与 B->A）：留 quote 更长的一条
    rels, seen = [], set()
    for key, r in best.items():
        if key in seen:
            continue
        if r["t"] in ("parent", "master"):
            rev = (r["b"], r["a"], r["t"])
            if rev in best:
                other = best[rev]
                if len(other["q"]) > len(r["q"]):
                    r = other
                seen.add(rev)
        rels.append(r)
        seen.add(key)
    dropped["去重"] = len(raw_rels) - len(rels)

    # --- master 精度：模型常把「隶属/心腹/上下级」标成师承，没有师承字样的降级为 other ---
    if FIX_MASTER:
        for r in rels:
            if r["t"] == "master" and not MASTER_CUE.search(r["q"]):
                r["t"] = "other"
                dropped["master-非师承降级"] += 1
        # 降级后可能与已有 other 边重复，重新去重
        seen_k, out = {}, []
        for r in rels:
            k = (pair_key(r) if r["t"] in SYMMETRIC else (r["a"], r["b"])) + (r["t"],)
            if k in seen_k:
                dropped["去重"] += 1
                prev = seen_k[k]
                if len(r["q"]) > len(prev["q"]):
                    out[out.index(prev)] = r
                    seen_k[k] = r
            else:
                seen_k[k] = r
                out.append(r)
        rels = out

    # --- parent 精度：quote 里看不出亲子关系的（多半是女婿/兄弟被标成父子）丢掉 ---
    if KEEP_PARENT_CUE:
        kept_p = []
        for r in rels:
            if r["t"] != "parent" or PARENT_CUE.search(r["q"]):
                kept_p.append(r)
            else:
                dropped["parent-无亲子线索"] += 1
        rels = kept_p

    # --- ally 边精度过滤：quote 里必须能看出「同盟」线索，否则只是同场出现 ---
    if KEEP_ALLY_CUE:
        kept_ally = []
        for r in rels:
            if r["t"] != "ally" or other_cue_hit(r) or both_named(r):
                kept_ally.append(r)
            else:
                dropped["ally-无同盟线索"] += 1
        rels = kept_ally

    # --- 同一个人不能既是盟友又是敌人：这种对子两边都不可信，整对丢掉 ---
    if DROP_CONFLICT:
        ts = {}
        for r in rels:
            ts.setdefault(pair_key(r), set()).add(r["t"])
        bad = {k for k, v in ts.items() if {"ally", "enemy"} <= v}
        kept = []
        for r in rels:
            if pair_key(r) in bad:
                dropped["盟友/敌对冲突"] += 1
            else:
                kept.append(r)
        rels = kept

    # --- other 边精度过滤：quote 里没有任何「关系线索」的纯共现边丢掉 ---
    if KEEP_OTHER_CUE:
        kept_other = []
        for r in rels:
            if r["t"] != "other" or OTHER_CUE.search(r["q"]):
                kept_other.append(r)
            else:
                dropped["other-无关系线索"] += 1
        rels = kept_other

    # --- 手工校正：方向反了 / 类型错的亲缘边（逐条核过正文，见文件头常量表）---
    byname = {c["name"]: i for i, c in enumerate(clusters)}
    flip = {frozenset(k): v for k, v in KIN_FLIP.items()}
    retype = {(frozenset(k[:2]), k[2]): v for k, v in KIN_RETYPE.items()}
    label = {(frozenset(k[:2]), k[2]): v for k, v in KIN_LABEL.items()}
    drop = {(frozenset(k[:2]), k[2]) for k in EDGE_DROP_KIN}
    out = []
    for r in rels:
        na, nb = clusters[r["a"]]["name"], clusters[r["b"]]["name"]
        ukey = (frozenset((na, nb)), r["t"])
        if r["t"] in ("parent", "sibling", "spouse") and ukey in drop:
            dropped["亲缘手工剔除"] += 1
            continue
        if r["t"] == "parent" and frozenset((na, nb)) in flip:
            a2, b2 = flip[frozenset((na, nb))]           # 值就是正确的 (父, 子)
            if na != a2:
                r["a"], r["b"] = byname[a2], byname[b2]
                dropped["亲缘方向校正"] += 1
        if ukey in retype:
            r["t"], r["label"] = retype[ukey]
            r["pin"] = True                    # 校正过的边不许被后续裁剪丢掉
            dropped["亲缘类型校正"] += 1
        elif ukey in label:
            r["label"] = label[ukey]
            r["pin"] = True
            dropped["亲缘义兄弟标注"] += 1
        elif r.get("pin") is None and r["t"] == "parent" and frozenset((na, nb)) in flip:
            r["pin"] = True
        out.append(r)
    rels = out

    # 校正后可能撞出重复（方向修正后与已有的正确边重合），再压一次
    best2, seen2 = {}, set()
    for r in sorted(rels, key=lambda r: (0 if r.get("pin") else 1, -len(r["q"]), r["ch"])):
        k = (min(r["a"], r["b"]), max(r["a"], r["b"]), r["t"]) if r["t"] in SYMMETRIC \
            else (r["a"], r["b"], r["t"])
        if k in seen2:
            dropped["去重"] += 1
            continue
        seen2.add(k)
        best2[k] = r
    rels = list(best2.values())

    # --- generation：加权并查集（父子差 1 优先，夫妻/兄弟同代次之，冲突边丢弃）---
    gp = list(range(len(clusters)))
    pot = [0] * len(clusters)

    def gfind(x):
        if gp[x] == x:
            return x
        p = gp[x]
        r = gfind(p)
        pot[x] += pot[p]
        gp[x] = r
        return r

    def gunion(a, b, d):                      # level(b) - level(a) = d
        ra, rb = gfind(a), gfind(b)
        if ra == rb:
            return (pot[b] - pot[a]) == d
        if ra < rb:
            gp[rb] = ra
            pot[rb] = pot[a] + d - pot[b]
        else:
            gp[ra] = rb
            pot[ra] = pot[b] - d - pot[a]
        return True

    worn, conf_nodes = set(), set()
    for r in rels:                            # 父子边先占位（可信度高于「兄弟」）
        if r["t"] != "parent":
            continue
        if not gunion(r["a"], r["b"], 1):
            worn.add(id(r))
            conf_nodes.update((r["a"], r["b"]))
            dropped["亲缘同代矛盾"] += 1
    for r in rels:                            # 夫妻/兄弟同代
        if r["t"] not in ("spouse", "sibling"):
            continue
        if not gunion(r["a"], r["b"], 0):
            worn.add(id(r))
            conf_nodes.update((r["a"], r["b"]))
            dropped["亲缘同代矛盾"] += 1
    rels = [r for r in rels if id(r) not in worn]
    dropped["父子代差>1"] = 0

    groups = defaultdict(list)
    for i in range(len(clusters)):
        groups[gfind(i)].append(i)
    level = {}
    for members in groups.values():           # 每个亲缘连通块从 0 起跳
        base = min(pot[i] for i in members)
        for i in members:
            level[i] = pot[i] - base

    # --- 出场区间 ---
    for c in clusters:
        toks = [t for t in [c["name"]] + c["aliases"] if len(t) >= 2]
        appear = [n for n in book.ns if any(t in book.raw[n] for t in toks)]
        c["firstSeen"] = appear[0] if appear else TOTAL_CHAPTERS
        c["lastSeen"] = appear[-1] if appear else TOTAL_CHAPTERS

    # --- role 里点名的势力补 faction（如 role=赤心队首领 -> 赤心队）---
    for c in clusters:
        if c["faction"] is None:
            for fname in sorted(FACTION_KIND, key=len, reverse=True):
                if fname in (c["role"] or ""):
                    c["faction"] = fname
                    break

    # --- 规模收敛：留关键人物 / 压单点度数 / 限关系总数（含孤立点剔除）---
    gen_by_old = {k: level.get(k, 0) for k in range(len(clusters))}
    conf_by_old = {k: k in conf_nodes for k in range(len(clusters))}
    old_clusters = clusters
    must_keep = set()
    for _f, _led in SEED_FACTIONS:
        _k = resolve(_led)
        if _k is not None:
            must_keep.add(_k)
    for _nm in KEEP_CHARS:                    # 手工校正过的人物不许被裁掉
        _k = resolve(_nm)
        if _k is not None:
            must_keep.add(_k)
    clusters, rels, remap = prune_for_demo(clusters, rels, dropped, must_keep)
    old_of = [old for old, _ in sorted(remap.items(), key=lambda kv: kv[1])]
    gen_new = {new: gen_by_old[old] for new, old in enumerate(old_of)}
    conf_new = {new: conf_by_old[old] for new, old in enumerate(old_of)}

    # --- id 分配（按出场先后）---
    order = sorted(range(len(clusters)), key=lambda i: (clusters[i]["firstSeen"], i))
    cid = {}
    for k, i in enumerate(order):
        clusters[i]["id"] = f"c{k + 1}"
        cid[i] = f"c{k + 1}"

    degree = Counter()
    for r in rels:
        degree[r["a"]] += 1
        degree[r["b"]] += 1

    # --- factions ---
    # 裁剪后重建「名字 -> 下标」表（resolve 闭包指向的是裁剪前的 clusters）
    tok2c2 = {}
    for i, c in enumerate(clusters):
        for t in [c["name"]] + c["aliases"]:
            tok2c2.setdefault(t, i)
    long2 = sorted((t for t in tok2c2 if len(t) >= 3), key=len, reverse=True)

    def resolve2(name):
        nm = (name or "").strip().strip("\"'“”‘’「」")
        if nm in tok2c2:
            return tok2c2[nm]
        for t in long2:
            if t in nm:
                return tok2c2[t]
        return None

    fac_members = defaultdict(set)
    for i, c in enumerate(clusters):
        if c["faction"] in FACTION_KIND:
            fac_members[c["faction"]].add(i)
        for f, pat in ROLE_FACTION_CUE:            # role 点名的役属势力也算成员
            if f != c["faction"] and re.search(pat, c["role"] or ""):
                fac_members[f].add(i)
    declared = defaultdict(Counter)
    for b in batches:
        for f in b.get("factions") or []:
            std = FACTION_MAP.get((f.get("name") or "").strip())
            led = (f.get("leader") or "").strip()
            if std and led:
                k = resolve2(led)
                if k is not None:
                    declared[std][k] += 1
                    fac_members[std].add(k)        # 声明出来的领袖本身就是成员
    for f, led in SEED_FACTIONS:                   # 正文里有、票数却不够的小势力
        k = resolve2(led)
        if k is not None:
            fac_members[f].add(k)
    chosen = [f for f in fac_members if fac_members[f]]
    chosen.sort(key=lambda f: (-len(fac_members[f]), f))
    chosen = chosen[:MAX_FACTIONS]
    out_factions, fid = [], {}
    for f in chosen:
        mem = sorted(fac_members[f], key=lambda i: (-degree[i], i))
        rank_of = {}
        for i in mem:
            r = clusters[i]["role"] or ""
            if f not in r:
                continue
            if re.search(r"皇帝|国主|国王|太祖", r):
                pr = 3
            elif LEADER_WORD.search(r):
                pr = 2
            else:
                pr = 1
            if pr >= 2 and re.search(r"前军|左军|右军|中军|后军|副都统|副统领|副帅|副使", r):
                pr -= 0.5
            rank_of[i] = (pr, degree[i])
        votes = [(c, n) for c, n in declared[f].most_common() if c in fac_members[f]]
        leader = None
        if rank_of and max(rank_of.values())[0] >= 2:   # 1) role 里「势力名 + 首领词」
            leader = max(rank_of, key=lambda i: rank_of[i])
        if leader is None and len(votes) > 1 and votes[0][1] >= 2:  # 2) 多批一致声明
            leader = votes[0][0]
        if leader is None and votes:                # 3) 单票声明
            leader = votes[0][0]
        if leader is None and mem:                  # 4) 度数最高者
            leader = mem[0]
        for sf, sled in SEED_FACTIONS:              # 5) 正文可查的小势力领袖兜底
            if sf == f:
                k = resolve2(sled)
                if k is not None and k in fac_members[f]:
                    leader = k
        fid[f] = f"f{len(fid) + 1}"
        out_factions.append({"id": fid[f], "name": f, "kind": FACTION_KIND[f],
                             "leader": cid[leader] if leader is not None else None,
                             "members": [cid[i] for i in mem]})
    mem_of = {i: sorted(fs, key=lambda f: fid[f]) for i, fs in
              ((i, {f for f in chosen if i in fac_members[f]}) for i in range(len(clusters)))}

    # --- 称谓补全（别名只补正文里确实出现过的写法）---
    for i in range(len(clusters)):
        drop = ALIAS_DROP.get(clusters[i]["name"])
        if drop:
            clusters[i]["aliases"] = [a for a in clusters[i]["aliases"] if a not in drop]
        have = set(clusters[i]["aliases"]) | {clusters[i]["name"]}
        for a in EXTRA_ALIASES.get(clusters[i]["name"], []):
            if a in have:
                continue
            if any(a in book.raw[n] for n in book.ns):
                clusters[i]["aliases"].append(a)
                have.add(a)

    # --- characters / relations / memberships ---
    out_chars = []
    for i in order:
        c = clusters[i]
        role = c["role"] if c["role"] not in BAD_ROLE else (
            f"{c['faction']}属员" if c["faction"] else "正文提及人物")
        out_chars.append({
            "id": c["id"], "name": c["name"], "aliases": c["aliases"],
            "gender": c["gender"] or "未知",
            "role": role,
            "faction": fid[mem_of[i][0]] if mem_of[i] else None,
            "generation": gen_new[i],
            "firstSeen": c["firstSeen"], "lastSeen": c["lastSeen"],
            "conflict": conf_new[i],
        })
    cmap = {c["id"]: c for c in out_chars}

    out_rels = []
    for r in sorted(rels, key=lambda r: (r["ch"], r["a"])):
        out_rels.append({"from": cid[r["a"]], "to": cid[r["b"]], "type": r["t"],
                         "label": r.get("label") or LABEL[r["t"]], "from_chapter": r["ch"],
                         "to_chapter": None, "quote": r["q"]})

    out_mem = []
    for i in order:
        c = clusters[i]
        if not mem_of[i]:
            continue
        to = c["lastSeen"] if TOTAL_CHAPTERS - c["lastSeen"] >= GAP else None
        for f in mem_of[i]:                        # 一人可有多重役属（岳飞：宋廷 + 岳家军）
            out_mem.append({"character": cid[i], "faction": fid[f],
                            "from": c["firstSeen"], "to": to})

    # 岳父/女婿（正文核实后改成 other + label）：辈分上晚辈低一代
    for r in out_rels:
        if r["type"] == "other" and r["label"] in ("岳父", "女婿"):
            cmap[r["to"]]["generation"] = cmap[r["from"]]["generation"] + 1

    # 关系时间区间：两端都出场之后才成立；最近一端退场后再过 GAP 章收口
    for r in out_rels:
        fa, fb = cmap[r["from"]], cmap[r["to"]]
        last = max(fa["lastSeen"], fb["lastSeen"])
        r["to_chapter"] = last if TOTAL_CHAPTERS - last >= GAP else None

    sampled = len(json.load(open(META))["covered"]) if os.path.exists(META) else 0
    data = {
        "meta": {"book": BOOK, "totalChapters": TOTAL_CHAPTERS,
                 "sampledChapters": sampled,
                 "generatedAt": __import__("datetime").datetime.now()
                 .astimezone().strftime("%Y-%m-%dT%H:%M:%S%z"),
                 "schema": SCHEMA},
        "characters": out_chars,
        "factions": out_factions,
        "memberships": out_mem,
        "relations": out_rels,
        "stats": {"characters": len(out_chars), "relations": len(out_rels),
                  "factions": len(out_factions), "chaptersScanned": len(chapters),
                  "sampledChapters": sampled},
    }
    g = data["meta"]["generatedAt"]
    data["meta"]["generatedAt"] = g[:-2] + ":" + g[-2:]

    # ---------------- 自检 + 写出 ----------------
    json.dump(data, open(OUT, "w"), ensure_ascii=False, indent=1)
    chk = json.load(open(OUT))
    ids = [c["id"] for c in chk["characters"]]
    assert len(ids) == len(set(ids)), "人物 id 重复"
    fids = [f["id"] for f in chk["factions"]]
    assert len(fids) == len(set(fids)), "势力 id 重复"
    idset, fidset = set(ids), set(fids)
    seen = set()
    for r in chk["relations"]:
        assert r["from"] in idset and r["to"] in idset, f"悬空关系 {r}"
        assert r["from"] != r["to"], f"自环 {r}"
        key = (min(r["from"], r["to"]), max(r["from"], r["to"]), r["type"]) \
            if r["type"] in SYMMETRIC else (r["from"], r["to"], r["type"])
        assert key not in seen, f"重复关系 {r}"
        seen.add(key)
        assert r["quote"] and len(r["quote"]) <= 20, f"quote 超长/缺失 {r}"
        assert 1 <= r["from_chapter"] <= TOTAL_CHAPTERS, f"章号越界 {r}"
    for m in chk["memberships"]:
        assert m["character"] in idset and m["faction"] in fidset, f"悬空 membership {m}"
        assert m["to"] is None or m["from"] <= m["to"], f"membership 倒挂 {m}"
    for f in chk["factions"]:
        assert f["members"], f"空势力 {f['id']}"
        assert f["leader"] in idset, f"leader 悬空 {f['id']}"
        for m in f["members"]:
            assert m in idset
    deg2 = Counter()
    for r in chk["relations"]:
        deg2[r["from"]] += 1
        deg2[r["to"]] += 1
    assert len(deg2) == len(chk["characters"]), "存在孤立点"

    cnt = Counter(r["type"] for r in chk["relations"])
    top = deg2.most_common(3)
    print("OK ->", OUT)
    print(json.dumps(chk["stats"], ensure_ascii=False))
    print("types:", dict(cnt))
    print("family(parent+sibling+spouse+master):",
          cnt["parent"] + cnt["sibling"] + cnt["spouse"] + cnt["master"])
    print("ally share: %.1f%%" % (100.0 * cnt["ally"] / max(1, len(chk["relations"]))))
    print("max degree:", [(cmap[i]["name"], d, "%.1f%%" % (100.0 * d / len(chk["relations"])))
                          for i, d in top])
    print("dropped:", dict(dropped))
    print("ambiguous aliases dropped:", len(ambiguous))


if __name__ == "__main__":
    main()
