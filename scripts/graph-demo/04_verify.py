#!/usr/bin/env python3
"""graph-demo-data.json 交付前自检：硬指标全部断言一遍，失败即非零退出。

用法：python3 scripts/graph-demo/04_verify.py [json路径]
"""
import json
import re
import sys
from collections import Counter, defaultdict

PATH = sys.argv[1] if len(sys.argv) > 1 else \
    "/home/hehecat/code/reader/frontend/public/graph-demo-data.json"

REL_TYPES = {"parent", "spouse", "sibling", "master", "disciple", "ally", "enemy", "other"}
SYMMETRIC = {"sibling", "spouse", "ally", "enemy"}
KIN = {"parent", "sibling", "spouse", "master"}
KIND_OK = {"门派", "家族", "势力", "组织"}

fails = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + (("  -> " + detail) if detail else ""))
    if not ok:
        fails.append(name)


data = json.load(open(PATH, encoding="utf-8"))
chars = data["characters"]
rels = data["relations"]
facs = data["factions"]
mem = data["memberships"]
cid = {c["id"] for c in chars}
fid = {f["id"] for f in facs}
by_id = {c["id"]: c for c in chars}

print(f"file: {PATH}")
print(f"meta: {data['meta']}")
print(f"stats: {data['stats']}")
print()

# ---- 1. 规模 ----
n, m = len(chars), len(rels)
check("人物数 ≥110", n >= 110, f"{n}")
check("人物数落在 120–140（父 agent 定稿区间）", 120 <= n <= 140, f"{n}")
check("关系数 ≥220", m >= 220, f"{m}")
check("关系数落在 240–300", 240 <= m <= 300, f"{m}")

# ---- 2. 类型分布 ----
tc = Counter(r["type"] for r in rels)
print("type counts:", dict(tc))
kin = sum(tc[t] for t in KIN)
check("亲缘/师承类 ≥45", kin >= 45, f"parent+sibling+spouse+master={kin}")
check("parent ≥18", tc["parent"] >= 18, f"{tc['parent']}")
check("enemy ≥45", tc["enemy"] >= 45, f"{tc['enemy']}")
check("ally ≤55% 总数", tc["ally"] <= 0.55 * m, f"{tc['ally']}/{m}={tc['ally'] / m:.1%}")
check("所有 type 都在枚举里", set(tc) <= REL_TYPES, str(set(tc)))

# ---- 3. 孤立点 ----
deg = Counter()
for r in rels:
    deg[r["from"]] += 1
    deg[r["to"]] += 1
iso = [c["id"] for c in chars if deg[c["id"]] == 0]
check("孤立点 = 0", not iso, f"{len(iso)} {iso[:5]}")

# ---- 4. 中心度 ----
top = deg.most_common(3)
ratio = top[0][1] / m if m else 0
print("max degree:", [(by_id[i]["name"], d, f"{d / m:.1%}") for i, d in top])
check("最大度数占比 <12%", ratio < 0.12, f"{ratio:.1%}")

# ---- 5. 端点/自环/重复 ----
bad_end = [r for r in rels if r["from"] not in cid or r["to"] not in cid]
check("所有关系端点都在 characters 里", not bad_end, f"{len(bad_end)}")
loops = [r for r in rels if r["from"] == r["to"]]
check("无自环", not loops, f"{len(loops)}")
seen, dup = set(), []
for r in rels:
    a, b = r["from"], r["to"]
    key = (min(a, b), max(a, b), r["type"]) if r["type"] in SYMMETRIC else (a, b, r["type"])
    if key in seen:
        dup.append(key)
    seen.add(key)
check("无重复关系(无向类型 A→B 与 B→A 视为同一条)", not dup, f"{len(dup)} {dup[:3]}")

# ---- 6. 出处 ----
noq = [r for r in rels if not (r.get("quote") or "").strip()]
check("每条关系都有 quote", not noq, f"{len(noq)}")
longq = [r for r in rels if len(r.get("quote") or "") > 20]
check("quote ≤20 字", not longq, f"{len(longq)} {[r['quote'] for r in longq[:3]]}")
badch = [r for r in rels
         if not isinstance(r.get("from_chapter"), int) or not 1 <= r["from_chapter"] <= 395]
check("from_chapter 是 1..395 的真实章号", not badch, f"{len(badch)}")

# ---- 7. 势力 ----
check("势力数 6–10", 6 <= len(facs) <= 10, f"{len(facs)}")
check("faction.kind 都在 {门派|家族|势力|组织}", all(f["kind"] in KIND_OK for f in facs),
      str({f["kind"] for f in facs}))
bad_lead = [f["name"] for f in facs if f["leader"] and f["leader"] not in cid]
check("faction.leader 引用有效", not bad_lead, str(bad_lead))
lead_not_mem = [f["name"] for f in facs if f["leader"] and f["leader"] not in f["members"]]
check("faction.leader 是本势力成员", not lead_not_mem, str(lead_not_mem))
bad_mem = [f["name"] for f in facs if not f["members"] or any(x not in cid for x in f["members"])]
check("faction.members 非空且引用有效", not bad_mem, str(bad_mem))
bad_cfac = [c["name"] for c in chars if c["faction"] and c["faction"] not in fid]
check("character.faction 引用有效", not bad_cfac, f"{len(bad_cfac)} {bad_cfac[:3]}")
bad_memref = [x for x in mem if x["character"] not in cid or x["faction"] not in fid]
check("membership 引用有效", not bad_memref, f"{len(bad_memref)}")
check("每个 faction 至少 1 条 memberships",
      all(any(x["faction"] == f["id"] for x in mem) for f in facs), "")

# ---- 8. generation ----
gen = {c["id"]: c["generation"] for c in chars}
bad_parent = [(by_id[r["from"]]["name"], by_id[r["to"]]["name"],
               gen[r["from"]], gen[r["to"]]) for r in rels
              if r["type"] == "parent" and gen[r["from"]] != gen[r["to"]] - 1]
check("parent 边代差恒为 1（父 = 子 - 1）", not bad_parent, str(bad_parent[:5]))
bad_same = [(by_id[r["from"]]["name"], by_id[r["to"]]["name"], r["type"])
            for r in rels
            if r["type"] in ("sibling", "spouse") and gen[r["from"]] != gen[r["to"]]]
check("sibling/spouse 同代", not bad_same, str(bad_same[:5]))

# 同一家族内不允许"父子同代"：沿 parent 边传递闭包，检查每对祖先-后代严格递减
parent_edges = defaultdict(list)
for r in rels:
    if r["type"] == "parent":
        parent_edges[r["from"]].append(r["to"])
viol = []
for p, kids in parent_edges.items():
    stack = [(k, 1) for k in kids]
    while stack:
        node, depth = stack.pop()
        if gen[p] != gen[node] - depth:
            viol.append((by_id[p]["name"], by_id[node]["name"], depth))
        stack += [(k, depth + 1) for k in parent_edges.get(node, [])]
check("多代祖先-后代代差自洽", not viol, str(viol[:5]))

# ---- 9. role / aliases ----
BAD_ROLE = {"", "主角", "配角", "人物", "群演"}
bad_role = [c["name"] for c in chars if c["role"].strip() in BAD_ROLE]
check("没人写「配角/主角」这类空身份", not bad_role, str(bad_role[:5]))
no_alias = [c["name"] for c in chars if not c["aliases"]]
print(f"       aliases 覆盖: {n - len(no_alias)}/{n} 有别名，平均 {sum(len(c['aliases']) for c in chars) / n:.1f} 个")
check("赵玖 收全常见称谓", {"赵官家", "官家", "大家", "陛下", "赵老九"} <=
      set(by_id[[c["id"] for c in chars if c["name"] == "赵玖"][0]]["aliases"]), "")

# ---- 10. schema 字段齐备 ----
need_c = {"id", "name", "aliases", "gender", "role", "faction", "generation",
          "firstSeen", "lastSeen", "conflict"}
need_r = {"from", "to", "type", "from_chapter"}
miss = [c["id"] for c in chars if not need_c <= set(c)]
check("character 字段齐备", not miss, str(miss[:3]))
miss = [(r["from"], r["to"]) for r in rels if not need_r <= set(r)]
check("relation 字段齐备", not miss, str(miss[:3]))
check("meta.schema 仍是 graph-demo-v1", data["meta"]["schema"] == "graph-demo-v1", "")

# ---- 11. parent 边方向：quote 里的亲缘字眼不能跟方向相反 ----
SENIOR_CUE = re.compile(r"其父|乃父|生父|其母|生母|父亲|家父|爹爹")      # 紧跟其后的名字 = 长辈
JUNIOR_CUE = re.compile(r"长子|次子|幼子|其子|之子|亲女|女儿|其女")      # 紧邻其前的名字 = 长辈
STEP_KIN = re.compile(r"女婿|岳父|岳丈|嫡母|继母|庶母|正宫|侄|外甥|族兄|堂兄|表兄|乳母")
KIN_EXEMPT = {("吴近", "吴夫人"): "「吴氏岳父吴近」是赵玖视角的岳父，吴近确是吴夫人之父"}
bad_dir = []
for r in rels:
    if r["type"] != "parent":
        continue
    pn, cn = by_id[r["from"]]["name"], by_id[r["to"]]["name"]
    q = r["quote"] or ""
    hit = None
    for m in SENIOR_CUE.finditer(q):                 # 「其父X」：X 是长辈，不能是子女
        after = q[m.end():m.end() + 8]
        if cn in after and pn not in after:
            hit = f"「{m.group()}」后面的 {cn} 是子女位"
            break
    if not hit:
        for m in JUNIOR_CUE.finditer(q):             # 「X长子」：X 是长辈，不能是子女
            before = q[max(0, m.start() - 12):m.start()]
            after = q[m.end():m.end() + 8]
            if cn in before and pn not in before and pn not in after:
                hit = f"「{m.group()}」前面的 {cn} 是长辈位"
                break
            if pn in after and cn not in after:
                hit = f"「{m.group()}」后面的 {pn} 是子女位"
                break
    if not hit:
        m = STEP_KIN.search(q)
        if m and (pn, cn) not in KIN_EXEMPT:
            hit = f"quote 出现「{m.group()}」不是亲子"
    if hit:
        bad_dir.append((pn, cn, q, hit))
check("parent 边方向与 quote 亲缘字眼一致（无「其父/乃父/生母/长子/次子」反例）",
      not bad_dir, str(bad_dir[:6]))

# ---------------------------------------------------------------- v2 追加断言
# 上一版的三处硬伤：enemy 占 56% 过度偏斜、势力碎成单成员、亲缘/师承太少。
# 这里把「改好之后应达到」的指标也钉住，防止下一轮又滑回去。
tc = Counter(r["type"] for r in rels)
n_rel = len(rels)
check("v2: enemy 占比 ≤40%（原来 56%）",
      tc["enemy"] <= 0.40 * n_rel, f"{tc['enemy']}/{n_rel} = {tc['enemy'] / n_rel:.1%}")
check("v2: parent ≥30", tc["parent"] >= 30, str(tc["parent"]))
check("v2: master+disciple ≥18（师承双向编码）",
      tc["master"] + tc["disciple"] >= 18, f"{tc['master']}+{tc['disciple']}")
check("v2: sibling ≥20", tc["sibling"] >= 20, str(tc["sibling"]))
check("v2: spouse ≥6", tc["spouse"] >= 6, str(tc["spouse"]))
check("v2: 关系总数 240–300", 240 <= n_rel <= 300, str(n_rel))
check("v2: 人物 130–150", 130 <= len(chars) <= 150, str(len(chars)))

facs = data["factions"]
check("v2: 势力数 6–9", 6 <= len(facs) <= 9, str(len(facs)))
BIG = {"宋廷", "金国", "御营", "岳家军", "西夏", "伪齐"}
small = [(f["name"], len(f["members"])) for f in facs
         if f["name"] not in BIG and len(f["members"]) < 2]
check("v2: 没有单成员小势力（宋廷/金国/御营/岳家军/西夏/伪齐 除外）", not small, str(small))
nm = {f["id"]: f["name"] for f in facs}
dist = Counter(nm[c["faction"]] for c in chars if c.get("faction"))
print("      势力人数:", dict(dist.most_common()))
nulls = [c["name"] for c in chars if not c.get("faction")]
check("v2: 无势力归属的人物 ≤3", len(nulls) <= 3, str(nulls))

print()
if fails:
    print(f"FAILED {len(fails)}: {fails}")
    sys.exit(1)
print("ALL CHECKS PASSED")
