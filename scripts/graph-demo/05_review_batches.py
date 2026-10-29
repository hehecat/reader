#!/usr/bin/env python3
"""v2 专批：①敌对关系复核 ②势力归属 ③亲缘补抽 的请求体构建。

输入：frontend/public/graph-demo-data.json（上一轮产物，只读）、.tmp/gd/chapters.json
输出：.tmp/graph-data-v2/req/review_*.json 以及 meta_review_*.json（记录该批问了什么）
"""
import json, os, re, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from gdlib import Chapters, BOOK_JSON, V2

OUTDIR = os.path.join(V2, "req")
MODEL = "deepseek-v4.1-flash"
MAXTOK = 8000
BATCH_EDGES = 26

FACTIONS = ["宋廷", "金国", "御营", "岳家军", "西夏", "伪齐"]

SYS_COMMON = ("你在核对一部中文历史小说的人物关系图谱抽取结果。"
              "小说是《绍宋》（北宋灭亡后赵玖建立南宋）。"
              "抽取脚本由模型批量生成，存在过度标注敌对、势力归属缺失的问题，你负责按正文复核。")


def ctx_for(chap, A, B, cap=340):
    """找同时提到 A、B 的句子，返回 (章号, 片段)。"""
    best = None
    for n in chap.ns:
        raw = chap.raw[n]
        if not any(a in raw for a in A) or not any(b in raw for b in B):
            continue
        sents = chap.sentences(n)
        for i, (s, e, seg) in enumerate(sents):
            ha = [a for a in A if a in seg]
            hb = [b for b in B if b in seg]
            if ha and hb:
                lo = max(0, i - 1)
                hi = min(len(sents), i + 2)
                txt = ""
                for j in range(lo, hi):
                    txt += sents[j][2] + ("。" if j + 1 < hi else "")
                    if len(txt) > cap:
                        break
                txt = txt.strip()[:cap]
                if best is None or len(txt) < len(best[1]):
                    best = (n, txt)
                break
        if best:
            break
    return best


def build_enemy(chap, data, outdir):
    by = {c["id"]: c for c in data["characters"]}
    fn = {f["id"]: f["name"] for f in data["factions"]}
    edges = [r for r in data["relations"] if r["type"] == "enemy"]
    items, meta = [], []
    for k, r in enumerate(edges):
        a, b = by[r["from"]], by[r["to"]]
        A = [a["name"]] + a["aliases"]
        B = [b["name"]] + b["aliases"]
        got = ctx_for(chap, A, B)
        if not got:                      # 没有同句共现的边不送审，保持原样
            continue
        items.append({
            "id": len(items),
            "甲": f"{a['name']}（{a['role']}，{fn.get(a['faction'], '无势力')}）",
            "乙": f"{b['name']}（{b['role']}，{fn.get(b['faction'], '无势力')}）",
            "原判据": f"第{r['from_chapter']}章「{r['quote']}」",
            "原文片段": got[1],
        })
        meta.append({"id": len(meta), "a": a["name"], "b": b["name"], "ch": got[0],
                     "old_quote": r["quote"], "fids": [r["from"], r["to"]]})
    tasks = []
    for i in range(0, len(items), BATCH_EDGES):
        chunk = items[i:i + BATCH_EDGES]
        prompt = (
            "下面每一条是图谱里被标成【敌对】的关系，附了两人在正文里同句出现的片段。\n"
            "请逐条判断两人在小说里的真实关系，从下列枚举里选一个：\n"
            "  enemy  = 真有敌对行为：战场厮杀、互相攻伐、加害/谋害、明确仇怨；\n"
            "  other  = 非敌对：君臣、上下级、部将/主公、同僚、旧识、姻亲、政见之争（弹劾、训斥、看不顺眼都算这里，不算敌对）；\n"
            "  ally   = 同盟/结盟/联手/共谋/真心追随效力；\n"
            "  parent / sibling / spouse / master / disciple = 仅当片段里明确写出亲缘或师承（如「其父」「长子」「之弟」「夫人」「恩师」「弟子」）。\n"
            "要求：\n"
            "1) 只依据给出的片段判断；片段没写敌对行为就不要再标 enemy，不要因为两人分属宋金两国就判敌对。\n"
            "   尤其注意：同朝为官、上下级、部将与主帅、君臣议事、政见之争、一方弹劾或训斥另一方，都属于 other，不是 enemy。\n"
            "2) quote 必须是片段里连续的一段原文，长度 ≤20 个汉字，且要能支撑你的判断（含「部将/同僚/弹劾/厮杀/结盟」这类字眼最好）。不许改写、不许拼接、不许加省略号。\n"
            "3) reason ≤12 字。\n"
            "只输出 JSON 数组，元素形如：{\"id\":0,\"type\":\"other\",\"quote\":\"原文片段\",\"reason\":\"上下级\"}\n\n"
            "待判条目：\n" + json.dumps(chunk, ensure_ascii=False, indent=1)
        )
        req = {"model": MODEL, "reasoning_effort": "none", "max_tokens": MAXTOK,
               "messages": [{"role": "system", "content": SYS_COMMON},
                            {"role": "user", "content": prompt}]}
        tasks.append((f"review_enemy_{i // BATCH_EDGES + 1:02d}", req, chunk))
    os.makedirs(outdir, exist_ok=True)
    for name, req, chunk in tasks:
        json.dump(req, open(os.path.join(outdir, name + ".json"), "w"), ensure_ascii=False)
    json.dump({"items": meta}, open(os.path.join(outdir, "meta_enemy.json"), "w"), ensure_ascii=False)
    print(f"enemy edges={len(items)} batches={len(tasks)}")
    return len(tasks)


def build_faction(chap, data, outdir):
    by = {c["id"]: c for c in data["characters"]}
    fn = {f["id"]: f["name"] for f in data["factions"]}
    # 需要归属的人：faction 为 null 的 + 单成员势力的唯一成员
    fac_size = {f["name"]: len(f["members"]) for f in data["factions"]}
    targets = []
    for c in data["characters"]:
        nm = fn.get(c["faction"])
        if c["faction"] is None:
            targets.append(c)
        elif nm and fac_size.get(nm, 0) <= 1:
            targets.append(c)
    items = []
    for k, c in enumerate(targets):
        chs = [n for n in chap.ns if c["name"] in chap.raw[n]]
        snip = ""
        for n in chs[:3]:
            raw = chap.raw[n]
            i = raw.find(c["name"])
            s = raw[max(0, i - 60):i + 90].replace("\n", " ")
            sents = [x for x in re.split(r"(?<=[。！？])", s) if x.strip()]
            snip += f"[第{n}章] " + "".join(sents).strip() + "\n"
        items.append({"id": k, "姓名": c["name"], "身份": c["role"],
                      "出场章": f"{c['firstSeen']}-{c['lastSeen']}（共{len(chs)}章）",
                      "常用称谓": c["aliases"][:6],
                      "原文线索": snip.strip()[:600]})
    prompt = (
        "下面这些人物在《绍宋》里没有可靠的势力归属，或所属势力只有他一个人。请按正文线索把他们归入下面 6 个势力之一：\n"
        "  宋廷（南宋朝廷/文官/东京留守司）、金国（金廷、金军、女真诸部）、御营（赵玖直辖的御营军各军，含御前班直）、\n"
        "  岳家军（岳飞本部）、西夏、伪齐（刘豫的齐国）。\n"
        "规则：\n"
        "1) faction 只能取上述 6 个名字之一，不要自创新势力。\n"
        "2) 以「他实际隶属/效忠/领哪家的粮」为准，不是「谁跟他打过仗」。如郭药师是燕京常胜军降将、归金国；刘晏的赤心队是御营前军一部，归御营。\n"
        "3) quote 必须是原文线索里连续的一段（≤20 字），能看出隶属关系；若线索不足就写空字符串并给出 confidence=low。\n"
        "只输出 JSON 数组：[{\"id\":0,\"faction\":\"御营\",\"quote\":\"原文\",\"reason\":\"≤12字\",\"confidence\":\"high|low\"}]\n\n"
        "待归类人物：\n" + json.dumps(items, ensure_ascii=False, indent=1)
    )
    req = {"model": MODEL, "reasoning_effort": "none", "max_tokens": MAXTOK,
           "messages": [{"role": "system", "content": SYS_COMMON},
                        {"role": "user", "content": prompt}]}
    json.dump(req, open(os.path.join(outdir, "review_faction_01.json"), "w"), ensure_ascii=False)
    json.dump({"items": [{"id": k, "name": c["name"], "fid": c["id"], "old": fn.get(c["faction"])}
                         for k, c in enumerate(targets)]},
              open(os.path.join(outdir, "meta_faction.json"), "w"), ensure_ascii=False)
    print(f"faction targets={len(items)}")
    return 1


if __name__ == "__main__":
    chap = Chapters()
    data = json.load(open(BOOK_JSON))
    n = 0
    n += build_enemy(chap, data, OUTDIR)
    n += build_faction(chap, data, OUTDIR)
    print("requests:", n, "->", OUTDIR)
