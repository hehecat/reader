#!/usr/bin/env python3
"""Step 2: 多批采样 + 拼装多个 LLM 抽取请求体。

改动（v2，原版是「前 30 章 + 均匀 50 章、单批 120k 字」）：
  * 全书 395 章切 6 个重叠窗口（窗口长 105 章、步长 58 章），
    保证开头 / 中段 / 结尾各阶段都被覆盖；
  * 窗口内按「人名密度」（种子人名出现次数 / 千字）排序取章，
    已采过的章在后续窗口里降权，让 6 批的章集基本不重复 ——
    总覆盖章数 ~240 章（旧版 80 章），人名密度高的章优先；
  * 每批正文上限 BUDGET=115k 字（旧版单批 120k），每章摘录 EXCERPT=2800 字；
  * 提示词强化：亲缘/师承必须逐条穷举、role 必须写身份（官职/阵营地位）、
    factions 6–10 个、每条关系必须有 <=20 字原文 quote + 真实章号。

输出（.tmp/gd/batches/）：
  req_01.json ... req_06.json   —— 每批一个完整请求体，逐个 curl
  sample_01.txt ... sample_06.txt
sample_meta.json 里记录各批选章与覆盖章数。
用法：python3 02_make_prompt.py
"""
import json
import os
import re

SRC = "/home/hehecat/code/.tmp/gd/chapters.json"
SEED = "/home/hehecat/code/reader/frontend/public/graph-demo-data.json"
OUTDIR = "/home/hehecat/code/.tmp/gd/batches"

BATCHES = 6          # 批数（每批一次独立请求）
WIN_LEN = 105        # 每个窗口覆盖的章数（相邻窗口重叠 ~47 章）
PER_BATCH = 40       # 每批取多少章
EXCERPT = 2800       # 每章摘录字数
BUDGET = 115_000     # 每批正文上限
MAXTOK = 16_000
NO_PROTO_BATCHES = 5   # 第三轮（排除主角赵玖）批数      # 单批输出上限（旧版 8000 装不下 100+ 条关系）
MODEL = "deepseek-v4.1-flash"

GENERIC = {"陛下", "官家", "大家", "相公", "太尉", "娘子", "殿下", "大王",
           "将军", "圣人", "足下", "老臣", "小底", "官人", "国主", "元帅"}

SYSTEM = "你是严谨的中文小说人物关系抽取器。你只输出 JSON，不输出任何解释、注释或 markdown 代码块。"

INSTRUCTION = """下面是长篇小说《{book}》的部分章节正文，每章以【第N章 标题】开头，N 是该章在全书的章序（1..395）。

请从这些正文中抽取人物关系图谱。严格遵守：

1. 只依据给定正文，不得使用你对该书或历史的先验知识补全；正文没写的关系不要写。
2. 同一人物的不同称呼（本名、字、号、官称、绰号、尊称）必须合并为一个人物，放进 aliases；
   每个主要人物的 aliases 尽量收全（本名/字/号/官称/绰号）。官称若被多人共用（如「太尉」「相公」）不要当别名。
3. 人物 45–70 个，只收有实际戏份或有明确关系的人物；纯路人不要。
   role 必须写**身份/官职/阵营地位**（如「宋廷皇帝」「金国主帅」「宋廷宰相」「岳飞部将」
   「伪齐皇帝」「东京留守」「内侍省大押班」），**禁止**写「主角」「配角」「路人」「武将」这类空话。
4. 关系 100–160 条。type 只能是：
   - parent（父母 → 子女）、spouse（夫妻）、sibling（兄弟姐妹，含堂/表/族兄弟）、
     master（师父 → 徒弟）、disciple（徒弟 → 师父，一般不用，写 master 反向即可）、
     ally（同盟/亲近/同党）、enemy（敌对/交战/政敌）、other（其他明确关系）。
   - parent 方向必须是「父/母 → 子」，master 方向是「师父 → 徒弟」。
   - spouse/sibling/ally/enemy 无方向，写一端即可。
   - **亲缘关系必须逐条穷举**：正文里出现的父子、母子、兄弟姐妹、叔侄、翁婿、姻亲、
     族亲都要抽成边，不要因为戏份少而省略；同一家族里凡是能确定的都写。
   - **敌对关系要写全**：战场对手、政敌、弹劾与被弹劾、背叛与猜忌都算 enemy。
5. 每条关系都要有：
   - chapter：该关系成立/被明确写出的那一章的 N（必须是上面正文里出现过的章号）；
   - quote：<=20 字的**原文片段**（逐字摘录，不要改写、不要加引号），作为出处。
   **没有原文出处的关系不要输出。**
6. 门派/家族/势力/组织/朝廷机构都进 factions，必须 6–10 个，kind 只能从
   「门派|家族|势力|组织」里选；leader 填该势力领袖的人物 name（能填就填，不认识填 null）。
   同一势力只写一条，不要重复（如「宋廷」「大宋」「宋朝」只保留「宋廷」）。
7. 每个人物的 faction 填其所属势力的 name（须出现在 factions 里），无所属填 null。
8. 每个人物至少出现在一条关系里，不要输出孤立人物。
9. 只输出一个 JSON 对象，不要 markdown、不要解释。字段用 name（不要用 id），Python 侧会自己编号。

输出结构（示例仅示意格式，内容以正文为准）：
{{"characters":[{{"name":"张三","aliases":["三哥","张三哥"],"gender":"男","role":"云岚宗掌门","faction":"云岚宗"}}],
"factions":[{{"name":"云岚宗","kind":"门派","leader":"张三"}}],
"relations":[{{"from":"张三","to":"李四","type":"master","chapter":3,"quote":"张三纳头便拜"}}]}}

正文如下：
"""


# ---- 补充批（--extra）：只抽亲缘/师承 + 冷门势力归属 ------------------------
# 6 批通用抽取里 ally/enemy 天然占多数、亲缘边偏少（家族树视图立不住），
# 所以再加一批「只抽亲缘」的请求：选章按「亲缘句式密度」排，并强制覆盖
# 常胜军/郭药师/赤心队这类冷门势力的章，把家族边和势力归属补齐。
EXTRA_INSTRUCTION = """下面是长篇小说《{book}》的部分章节正文，每章以【第N章 标题】开头，N 是该章在全书的章序（1..395）。

本次任务**只抽亲缘与师承关系**，不要抽同盟(ally)、敌对(enemy)之类的关系。

请严格遵守：

1. 只依据给定正文，不得使用先验知识补全。
2. 人物 30–60 个；role 写身份/官职/阵营地位（如「宋廷宰相」「金国主帅」「岳飞长子」），
   禁止写「主角」「配角」「路人」。
3. **穷举亲缘**：正文里出现的每一种亲缘都要抽成边，包括且不限于
   父子、母子、兄弟、姐妹、叔侄、祖孙、翁婿、姻亲、族亲、义父子、养父子。
   - type=parent：父母 → 子女（方向必须「父/母 → 子」），关系远近都要写；
   - type=spouse：夫妻；type=sibling：兄弟姐妹（含堂/表/族兄弟）；
   - type=master：师父 → 徒弟（或明确的长官提携、拜入门下）；不确定就不要写。
   - 每个人物只要正文提到其亲属就写出来，哪怕戏份很少；可以只出现在这些亲缘关系里。
4. 每条关系必须给出：chapter（该关系被明确写出的那一章的 N，必须是上面出现过的章号）
   和 quote（<=20 字**原文片段**，逐字摘录，不要改写）。**没有原文出处的关系不要输出。**
5. factions 5–8 个，kind 只能从「门派|家族|势力|组织」里选，leader 能填就填；
   冷门势力（如常胜军）也要登记，人物 faction 填势力 name。
6. 只输出一个 JSON 对象，字段用 name：
{{"characters":[{{"name":"张三","aliases":["三哥"],"gender":"男","role":"云岚宗长老","faction":"云岚宗"}}],
"factions":[{{"name":"云岚宗","kind":"门派","leader":"张三"}}],
"relations":[{{"from":"张三","to":"李四","type":"parent","chapter":3,"quote":"张三之子"}}]}}

正文如下：
"""

KIN_CUE = re.compile(r"长子|次子|幼子|之子|之弟|之妻|女婿|侄子|侄儿|叔父|伯父|外甥|"
                     r"胞弟|族兄|堂兄|表兄|义父|养子|亲子|独子|之母|之女|之兄")
FORCE = re.compile(r"常胜军|郭药师|赤心队")


def build_extra(chapters, byn):
    """补充批：亲缘句式密度最高的章 + 冷门势力章，合成 req_07.json。"""
    score = {}
    for n, c in byn.items():
        txt = c["content"]
        score[n] = (4 * len(KIN_CUE.findall(txt)) + 3 * len(FORCE.findall(txt))) * 1000.0 / max(1, len(txt))
    force = sorted({n for n, c in byn.items() if FORCE.search(c["content"])})
    pool = sorted(byn, key=lambda n: score[n], reverse=True)
    picked = list(dict.fromkeys(force + pool))[:PER_BATCH]
    picked = sorted(picked)
    blocks = []
    for n in picked:
        c = byn[n]
        title = re.sub(r"^第[0-9零一二三四五六七八九十百千两]+章\s*", "", c["title"])
        blocks.append(f"\n【第{n}章 {title}】\n{c['content'][:EXCERPT].strip()}")
    text = "".join(blocks)
    prompt = EXTRA_INSTRUCTION.format(book="绍宋") + text
    req = {"model": MODEL, "max_tokens": MAXTOK, "reasoning_effort": "none",
           "messages": [{"role": "system", "content": SYSTEM},
                        {"role": "user", "content": prompt}]}
    json.dump(req, open(f"{OUTDIR}/req_07.json", "w"), ensure_ascii=False)
    open(f"{OUTDIR}/sample_07.txt", "w").write(text)
    print(f"extra: picked={len(picked)} text={len(text)} prompt={len(prompt)}")
    return {"batch": 7, "range": [picked[0], picked[-1]], "chapters": picked,
            "textChars": len(text), "promptChars": len(prompt)}


def build_changsheng(byn):
    """补充批 2（req_08）：常胜军/郭药师只在 6 章里出现过，通用 6 批里没抽到，
    这里把这 6 章及其前后各 2 章单独喂一遍，并要求登记常胜军。"""
    hit = sorted({n for n, c in byn.items() if re.search(r"常胜军|郭药师", c["content"])})
    nears = sorted({m for n in hit for m in range(n - 2, n + 3) if m in byn})
    blocks = []
    for n in nears:
        c = byn[n]
        title = re.sub(r"^第[0-9零一二三四五六七八九十百千两]+章\s*", "", c["title"])
        blocks.append(f"\n【第{n}章 {title}】\n{c['content'][:EXCERPT].strip()}")
    text = "".join(blocks)
    note = ("补充要求：本批正文集中在燕山/河北一带，务必把「常胜军」（郭药师旧部）这个势力\n"
            "及其首领、成员登记到 factions 里，人物 faction 填「常胜军」。\n\n")
    prompt = INSTRUCTION.format(book="绍宋") + note + text
    req = {"model": MODEL, "max_tokens": MAXTOK, "reasoning_effort": "none",
           "messages": [{"role": "system", "content": SYSTEM},
                        {"role": "user", "content": prompt}]}
    json.dump(req, open(f"{OUTDIR}/req_08.json", "w"), ensure_ascii=False)
    open(f"{OUTDIR}/sample_08.txt", "w").write(text)
    print(f"changsheng: hit={hit} picked={len(nears)} text={len(text)}")
    return {"batch": 8, "range": [nears[0], nears[-1]], "chapters": nears,
            "textChars": len(text), "promptChars": len(prompt)}



# ---- 第三轮（主角排除批）：只补「赵玖以外」的人之间的关系 -------------------
# 赵玖是全书主角，几乎和所有人都有边，度数会吃满整张图（中心度畸形）。
# 这一轮把他整个排除，逼模型去抽配角之间、敌方阵营内部、朝堂各派之间的关系。
NO_PROTO_INSTRUCTION = """下面是长篇小说《{book}》的部分章节正文，每章以【第N章 标题】开头，N 是该章在全书的章序（1..395）。

本次任务：抽取人物关系，但**必须排除主角赵玖**（含其称呼：官家、陛下、赵官家、大家、赵老九、九哥、康王、天子、圣天子、大元帅）。
即：characters 里不要出现赵玖；relations 里任何一条都不得以赵玖为端点。

只抽其他人之间的关系，重点是：
- 配角与配角之间的同盟、敌对、隶属、师承（如某将与其部将、某相与其门生）；
- 敌方阵营内部的关系（金国诸将之间、伪齐君臣之间、西夏嵬名宗室之间）；
- 朝堂各派系之间（主战/主和、台谏与宰执、内侍与外朝）；
- 亲缘（父子、兄弟、夫妻、叔侄、翁婿）必须逐条穷举，哪怕戏份很少。

其他要求：
1. 只依据正文，不得用先验知识补全。
2. 人物 40–70 个，role 必须写身份/官职/阵营地位，禁止「主角/配角/路人」。
3. 关系 90–150 条，type 只能是 parent / spouse / sibling / master / disciple / ally / enemy / other；
   parent 方向是「父/母 → 子」，master 是「师父 → 徒弟」。
4. 每条关系必须带 chapter（正文里出现过的章号）和 quote（<=20 字逐字原文片段），
   没有原文出处的不要输出。
5. factions 5–9 个，kind 只能从「门派|家族|势力|组织」里选，leader 能填就填。
6. 只输出一个 JSON 对象，字段用 name。
{{"characters":[{{"name":"张三","aliases":["三哥"],"gender":"男","role":"金国万户","faction":"金国"}}],
"factions":[{{"name":"金国","kind":"势力","leader":"完颜宗弼"}}],
"relations":[{{"from":"张三","to":"李四","type":"ally","chapter":3,"quote":"张三与李四同上"}}]}}

正文如下：
"""


def build_no_proto(byn, dens, n_batches, seq):
    """按人名密度从高到低取 n_batches*PER_BATCH 章，切成 n_batches 个请求。"""
    pool = sorted(byn, key=lambda n: dens[n], reverse=True)
    out = []
    for i in range(n_batches):
        picked = sorted(pool[i * PER_BATCH:(i + 1) * PER_BATCH])
        if not picked:
            break
        blocks = []
        for n in picked:
            c = byn[n]
            title = re.sub(r"^第[0-9零一二三四五六七八九十百千两]+章\\s*", "", c["title"])
            blocks.append(f"\\n【第{n}章 {title}】\\n{c['content'][:EXCERPT].strip()}")
        text = "".join(blocks)
        prompt = NO_PROTO_INSTRUCTION.format(book="绍宋") + text
        req = {"model": MODEL, "max_tokens": MAXTOK, "reasoning_effort": "none",
               "messages": [{"role": "system", "content": SYSTEM},
                            {"role": "user", "content": prompt}]}
        seq += 1
        json.dump(req, open(f"{OUTDIR}/req_{seq:02d}.json", "w"), ensure_ascii=False)
        open(f"{OUTDIR}/sample_{seq:02d}.txt", "w").write(text)
        print(f"no-proto batch {seq}: ch {picked[0]}-{picked[-1]} picked={len(picked)} text={len(text)}")
        out.append({"batch": seq, "range": [picked[0], picked[-1]], "chapters": picked,
                    "textChars": len(text), "promptChars": len(prompt)})
    return out, seq



# ---- 第四轮（阵营批）：按「金/伪齐/西夏/朝堂/军府」密度选章，且排除主角 ----------
CAMP_CUE = re.compile(r"金军|金国|女真|兀术|娄室|挞懒|粘罕|宗弼|宗磐|吴乞买|蒲卢浑|讹鲁补|"
                      r"西夏|李乾顺|嵬名|伪齐|刘豫|李成|郦琼|常胜军|郭药师")
COURT_CUE = re.compile(r"宰相|执政|台谏|御史|中书|门下|尚书|枢密|朝堂|弹劾|奏对|御前会议")
ALLY_ONLY_INSTRUCTION = """下面是长篇小说《{book}》的部分章节正文，每章以【第N章 标题】开头，N 是该章在全书的章序（1..395）。

本次任务：专门抽取**同盟/敌对/隶属**类关系（不要抽亲缘，亲缘已有专门批次），且必须排除主角赵玖（官家/陛下/大家/赵老九/康王/天子）。
即 characters 与 relations 里都不得出现赵玖。

规则：
1. 每条 ally（同盟/同党/联手/共事/互相扶持）的 quote 必须能看出同盟或亲近的来由，
   例如出现：心腹、亲信、结盟、相约、联手、同党、倚重、拥戴、麾下、部将、投效、归顺 等字样；
   仅仅是同场出现、对话、头衔提及的**不要**输出。
2. 每条 enemy（敌对/对抗/仇怨）的 quote 要能看出敌意或交战：见仗、对垒、生擒、攻、杀、降罪、弹劾、仇、逆贼 等。
3. 每条 other（隶属/上下级/君臣/荐举）的 quote 要能看出隶属来由：麾下、部将、荐、任用、节制 等。
4. 关系 90–140 条，人物 40–70 个，role 写身份。
5. 只依据正文，必须带 chapter（章号）和 quote（<=20 字逐字原文）。
6. 只输出一个 JSON 对象：
{{"characters":[{{"name":"张三","aliases":[],"gender":"男","role":"金国万户","faction":"金国"}}],
"factions":[{{"name":"金国","kind":"势力","leader":"完颜宗弼"}}],
"relations":[{{"from":"张三","to":"李四","type":"enemy","chapter":3,"quote":"张三与李四对阵"}}]}}

正文如下：
"""


def build_round(byn, score_fn, n_batches, seq, instruction, exclude=(), tag=""):
    """按 score_fn 选章，切 n_batches 个请求。"""
    pool = [n for n in byn if n not in exclude]
    pool.sort(key=score_fn, reverse=True)
    out = []
    for i in range(n_batches):
        picked = sorted(pool[i * PER_BATCH:(i + 1) * PER_BATCH])
        if not picked:
            break
        blocks = []
        for n in picked:
            c = byn[n]
            title = re.sub(r"^第[0-9零一二三四五六七八九十百千两]+章\\s*", "", c["title"])
            blocks.append(f"\\n【第{n}章 {title}】\\n{c['content'][:EXCERPT].strip()}")
        text = "".join(blocks)
        prompt = instruction.format(book="绍宋") + text
        req = {"model": MODEL, "max_tokens": MAXTOK, "reasoning_effort": "none",
               "messages": [{"role": "system", "content": SYSTEM},
                            {"role": "user", "content": prompt}]}
        seq += 1
        json.dump(req, open(f"{OUTDIR}/req_{seq:02d}.json", "w"), ensure_ascii=False)
        open(f"{OUTDIR}/sample_{seq:02d}.txt", "w").write(text)
        print(f"{tag} batch {seq}: ch {picked[0]}-{picked[-1]} picked={len(picked)} text={len(text)}")
        out.append({"batch": seq, "range": [picked[0], picked[-1]], "chapters": picked,
                    "textChars": len(text), "promptChars": len(prompt), "kind": tag})
    return out, seq


def seeds():
    """人名密度用的种子词：现有图谱里的 name + aliases。"""
    try:
        d = json.load(open(SEED))
    except Exception:
        return []
    toks = set()
    for c in d.get("characters", []):
        for t in [c.get("name")] + list(c.get("aliases") or []):
            if t and len(t) >= 2 and t not in GENERIC:
                toks.add(t)
    return sorted(toks, key=len, reverse=True)


def windows(lo, hi, n, win):
    if n == 1:
        return [(lo, hi)]
    step = max(1, (hi - lo + 1 - win) / (n - 1))
    out = []
    for i in range(n):
        a = lo + round(step * i)
        out.append((a, min(hi, a + win - 1)))
    return out


def main():
    chapters = json.load(open(SRC))
    byn = {c["n"]: c for c in chapters}
    hi = max(byn)
    toks = seeds()
    pat = re.compile("|".join(re.escape(t) for t in toks)) if toks else None

    # 人名密度：每千字命中的种子人名次数
    dens = {}
    for n, c in byn.items():
        txt = c["content"]
        hits = len(pat.findall(txt)) if pat else 0
        dens[n] = hits * 1000.0 / max(1, len(txt))

    os.makedirs(OUTDIR, exist_ok=True)
    chosen_all, metas = set(), []
    seq = 0
    for a, b in windows(1, hi, BATCHES, WIN_LEN):
        pool = [n for n in range(a, b + 1) if n in byn]
        # 已采过的章降权（不禁止，只是为了多覆盖没扫过的章）
        scored = sorted(pool, key=lambda n: (dens[n] - (6.0 if n in chosen_all else 0.0),
                                             -abs(n - (a + b) / 2)), reverse=True)
        picked = scored[:PER_BATCH]
        # 窗口首尾各留一章，保证阶段连续
        for edge in (a, b):
            if edge in byn and edge not in picked:
                picked[-1] = edge
        picked = sorted(set(picked))

        blocks = []
        for n in picked:
            c = byn[n]
            body = c["content"][:EXCERPT].strip()
            title = re.sub(r"^第[0-9零一二三四五六七八九十百千两]+章\s*", "", c["title"])
            blocks.append(f"\n【第{n}章 {title}】\n{body}")
        text = "".join(blocks)

        prompt = INSTRUCTION.format(book="绍宋") + text
        req = {
            "model": MODEL,
            "max_tokens": MAXTOK,
            # 该网关默认开思考，思考会吃光输出预算导致 content 为空 → 必须关掉
            "reasoning_effort": "none",
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": prompt},
            ],
        }
        seq += 1
        json.dump(req, open(f"{OUTDIR}/req_{seq:02d}.json", "w"), ensure_ascii=False)
        open(f"{OUTDIR}/sample_{seq:02d}.txt", "w").write(text)
        chosen_all |= set(picked)
        metas.append({"batch": seq, "range": [a, b], "chapters": picked,
                      "textChars": len(text), "promptChars": len(prompt)})
        print(f"batch {seq}: ch {a}-{b} picked={len(picked)} "
              f"text={len(text)} prompt={len(prompt)}")

    meta = {"batches": metas, "covered": sorted(chosen_all),
            "coveredCount": len(chosen_all), "excerpt": EXCERPT,
            "budget": BUDGET, "batches_n": BATCHES}
    # 补充批（亲缘/冷门势力 + 常胜军）也算进覆盖章数
    for extra in (build_extra(chapters, byn), build_changsheng(byn)):
        metas.append(extra)
        chosen_all |= set(extra["chapters"])
        seq = extra["batch"]          # 补充批占用了 07/08，第二轮从 09 继续编号

    # 第二轮：把第一轮没扫到的章按人名密度补齐（每批 PER_BATCH 章，通用抽取）
    rest = sorted((n for n in byn if n not in chosen_all),
                  key=lambda n: dens[n], reverse=True)
    for i in range(0, len(rest), PER_BATCH):
        picked = sorted(rest[i:i + PER_BATCH])
        blocks = []
        for n in picked:
            c = byn[n]
            title = re.sub(r"^第[0-9零一二三四五六七八九十百千两]+章\s*", "", c["title"])
            blocks.append(f"\n【第{n}章 {title}】\n{c['content'][:EXCERPT].strip()}")
        text = "".join(blocks)
        prompt = INSTRUCTION.format(book="绍宋") + text
        req = {"model": MODEL, "max_tokens": MAXTOK, "reasoning_effort": "none",
               "messages": [{"role": "system", "content": SYSTEM},
                            {"role": "user", "content": prompt}]}
        seq += 1
        json.dump(req, open(f"{OUTDIR}/req_{seq:02d}.json", "w"), ensure_ascii=False)
        open(f"{OUTDIR}/sample_{seq:02d}.txt", "w").write(text)
        chosen_all |= set(picked)
        metas.append({"batch": seq, "range": [picked[0], picked[-1]], "chapters": picked,
                      "textChars": len(text), "promptChars": len(prompt)})
        print(f"round2 batch {seq}: ch {picked[0]}-{picked[-1]} picked={len(picked)} "
              f"text={len(text)}")
    meta["batches"] = metas
    meta["covered"] = sorted(chosen_all)
    meta["coveredCount"] = len(chosen_all)
    # 第三轮：主角排除批（补「别人之间」的关系，压中心度）
    extra3, seq = build_no_proto(byn, dens, NO_PROTO_BATCHES, seq)
    metas.extend(extra3)
    for e in extra3:
        chosen_all |= set(e["chapters"])
    meta["batches"] = metas
    meta["covered"] = sorted(chosen_all)
    meta["coveredCount"] = len(chosen_all)
    # 第四轮：阵营/朝堂批（补金、伪齐、西夏、朝堂各派之间的关系，也压中心度）
    used3 = {n for e in extra3 for n in e["chapters"]}
    camp_fn = lambda n: (len(CAMP_CUE.findall(byn[n]["content"])) * 1000.0
                         / max(1, len(byn[n]["content"])))
    court_fn = lambda n: ((len(COURT_CUE.findall(byn[n]["content"])) * 0.8
                           + len(CAMP_CUE.findall(byn[n]["content"])) * 0.2) * 1000.0
                          / max(1, len(byn[n]["content"])))
    e4, seq = build_round(byn, camp_fn, 2, seq, NO_PROTO_INSTRUCTION, used3, "camp")
    e5, seq = build_round(byn, court_fn, 2, seq, NO_PROTO_INSTRUCTION,
                          used3 | {n for e in e4 for n in e["chapters"]}, "court")
    # 第五轮：同盟/敌对批（按「心腹/结盟/麾下/部将」密度选章，专抽 ally/enemy）
    ally_fn = lambda n: ((len(re.findall(r"心腹|亲信|结盟|联手|相约|同党|倚重|拥戴|麾下|部将|"
                                         r"投效|归顺", byn[n]["content"])) * 1000.0)
                         / max(1, len(byn[n]["content"])))
    used4 = used3 | {n for e in e4 + e5 for n in e["chapters"]}
    e6, seq = build_round(byn, ally_fn, 2, seq, ALLY_ONLY_INSTRUCTION, used4, "ally")
    for e in e4 + e5 + e6:
        metas.append(e)
        chosen_all |= set(e["chapters"])
    json.dump(meta, open("/home/hehecat/code/.tmp/gd/sample_meta.json", "w"),
              ensure_ascii=False)
    print(json.dumps({"coveredCount": len(chosen_all), "batches": len(metas),
                      "totalChars": sum(m["textChars"] for m in metas)},
                     ensure_ascii=False))


if __name__ == "__main__":
    main()
