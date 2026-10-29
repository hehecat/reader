/**
 * 搜索「静默态」(settling / quiet) 的纯状态机 —— 只描述视觉降级, 不碰结果数据.
 *
 * 为什么需要它: 后端 full_scan 单窗搜索实测 111 本书里 100 本在 42.5s 到齐, 最后 11 本
 * 要等到 110.7s; 收尾那 35~60s 是墙钟成本, 共识裁决不给 full_scan 加收口, 感知由前端承担.
 * 于是: 结果继续流入, 但不再用「还在搜索…」的整页等待态按住用户 —— 一段静默后把等待提示
 * 降级成轻提示, 一旦真有新结果立刻回到正常态并亮出「新到 N 条」.
 *
 * 与其他模块的分工(硬约束, 违反即丢结果/误报):
 * 1. 状态机只吃「新增条数 delta」, 从不读写结果数组: 晚到的结果照旧走
 *    mergeBatch/rankResults 追加去重, 一条不丢, 也不重置滚动位置/已选中项.
 * 2. 降级准入见 canSettle: 必须「仍在搜 + 没有错误 + 已有结果」, 所以
 *    无结果、出错、已结束都不可能进入静默态(不会把未完成的源说成有结果).
 * 3. 退出由调用方保证: 真结束(isEnd 到达 → searching=false)、停止、错误都会 reset;
 *    useSearchSSE 对外暴露的 quiet 还会再与 searching/error 相与, 任何路径都退得出来.
 *
 * 放在 components/search/ 下是为了能被 node --test 直接 import(无 `@/` 别名依赖),
 * 同时与搜索页的展示层同目录: 它只服务搜索结果区的等待提示.
 */

/** 连续无新增多久才降级为静默态 (ms): 802~1500 区间取中, 不抢首批结果的正常态 */
export const QUIET_AFTER_MS = 1_200;

/** 「新到 N 条」提示保留时长 (ms) */
export const QUIET_NOTICE_MS = 2_500;

export interface QuietState {
  /** 视觉静默态: 等待提示降级为轻提示, 列表照常可交互 */
  quiet: boolean;
  /** 静默被新结果打断后累计的新到条数 (>0 时提示「新到 N 条」) */
  arrived: number;
}

export const QUIET_INITIAL: QuietState = { quiet: false, arrived: 0 };

export type QuietEvent =
  /** 有新结果落地; delta = 本次新增条数 (纯游标推进时 delta 为 0) */
  | { type: "grow"; delta: number }
  /** 静默阈值到期(调用方已用 canSettle 判过准入) */
  | { type: "settle" }
  /** 「新到 N 条」提示到期收起 */
  | { type: "notice-clear" }
  /** 新搜索 / 恢复快照 / 停止 / 手动续搜: 回到初始正常态 */
  | { type: "reset" };

/** 静默态迁移; 状态没有变化时返回同一个对象(调用方据此跳过 setState) */
export function reduceQuiet(state: QuietState, event: QuietEvent): QuietState {
  switch (event.type) {
    case "grow": {
      if (event.delta <= 0) {
        // 只有游标推进/简介补全: 不是新结果, 不打断静默(否则收尾期会被反复踢回等待态)
        return state;
      }
      if (state.quiet) {
        // 静默里真的来了新结果: 立刻回到正常态, 并亮出这一批到达条数(不静默插入)
        return { quiet: false, arrived: event.delta };
      }
      if (state.arrived > 0) {
        // 已在「新到」提示窗口内: 同一波继续累加, 提示不闪断
        return { quiet: false, arrived: state.arrived + event.delta };
      }
      // 正常流式增长(首批到 90% 那段): 状态行本来就在涨, 不额外弹提示
      return state;
    }
    case "settle":
      return state.quiet ? state : { quiet: true, arrived: 0 };
    case "notice-clear":
      return state.arrived === 0 ? state : { quiet: state.quiet, arrived: 0 };
    case "reset":
      return state.quiet || state.arrived > 0 ? QUIET_INITIAL : state;
  }
}

/**
 * 静默态准入: 仍在搜索、已有结果、且这一轮没有报错.
 * 三者缺一都不降级 —— 尤其是「0 条结果」时不能显示「已收到 0 条, 剩余源仍在返回」.
 */
export function canSettle(input: {
  searching: boolean;
  errored: boolean;
  count: number;
}): boolean {
  return input.searching && !input.errored && input.count > 0;
}
