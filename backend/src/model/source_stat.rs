//! 书源置信度统计行 (source_stats 表)
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SourceStat {
    pub user_namespace: String,
    pub source_url: String,
    pub attempts: i64,
    pub successes: i64,
    /// 超时次数（含「被窗硬顶截断」的补记）。**不计入 successes，也不计入 `consec_failures`**：
    /// 超时是「慢」不是「死」，用它和连败区分开，避免把好源误判成死源。
    pub timeouts: i64,
    pub lat_sum_ms: i64,
    pub intro_hits: i64,
    pub intro_count: i64,
    pub toc_attempts: i64,
    pub toc_successes: i64,
    pub content_attempts: i64,
    pub content_successes: i64,
    pub consec_failures: i64,
    pub last_failure_at: i64,
    pub updated_at: i64,
}

/// 通道权重：搜索 45% / 目录 35% / 正文 20%。缺通道时按比例重归一（不抄上级、不补常数）。
const W_SEARCH: f64 = 0.45;
const W_TOC: f64 = 0.35;
const W_CONTENT: f64 = 0.20;
/// 几何池化地板：任一环全废也不归零，留"还有一口气"的痕迹
const POOL_FLOOR: f64 = 0.02;
/// Wilson 95% 双侧分位
const Z95: f64 = 1.959_963_984_540_054;
/// 速度：平均延迟超过 10s 才开始温和扣分，最多扣到 0.7（旧版整体压到 0.3 倍）
const LAT_SOFT_MS: f64 = 10_000.0;
const LAT_PENALTY_FLOOR: f64 = 0.7;
/// 连败滞回：前 2 次不罚，第 3 次起每次减半，最多 1/8（旧版连败 3 次一刀压到 0.05）
const STREAK_FREE: i64 = 2;
/// 判死门槛：有效样本（剔除超时）≥5、24h 内失败过、成功率 <20%
const DEAD_MIN_SAMPLES: i64 = 5;
const DEAD_WINDOW_MS: i64 = 24 * 3_600_000;
const DEAD_MAX_RATE: f64 = 0.2;

impl SourceStat {
    /// 单通道证据 → (Beta(½,½) 后验均值, Wilson 下界)；`n == 0` 返回 None（**没有证据就是没有**）
    ///
    /// 后验均值让 1/1 拿不到满分（0.75）、0/3 也不是 0；下界用于排序，小样本不冲顶。
    fn channel_evidence(ok: i64, n: i64) -> Option<(f64, f64)> {
        if n <= 0 {
            return None;
        }
        let n_f = n as f64;
        let ok_f = (ok as f64).clamp(0.0, n_f);
        let mean = (ok_f + 0.5) / (n_f + 1.0);
        let p = ok_f / n_f;
        let z2 = Z95 * Z95;
        let denom = 1.0 + z2 / n_f;
        let center = (p + z2 / (2.0 * n_f)) / denom;
        let half = Z95 * (p * (1.0 - p) / n_f + z2 / (4.0 * n_f * n_f)).sqrt() / denom;
        Some((mean, (center - half).max(0.0)))
    }

    /// 三通道证据：(权重, 后验均值, Wilson 下界)；无证据的通道整条消失
    fn channels(&self) -> Vec<(f64, f64, f64)> {
        [
            (W_SEARCH, self.successes, self.attempts),
            (W_TOC, self.toc_successes, self.toc_attempts),
            (W_CONTENT, self.content_successes, self.content_attempts),
        ]
        .into_iter()
        .filter_map(|(w, ok, n)| Self::channel_evidence(ok, n).map(|(m, lo)| (w, m, lo)))
        .collect()
    }

    /// 速度与连败的惩罚因子（点估计与下界共用，保证两者可比）
    fn penalties(&self) -> f64 {
        let avg_lat = self.lat_sum_ms as f64 / self.attempts.max(1) as f64;
        let mut f = 1.0;
        if avg_lat > LAT_SOFT_MS {
            f *= (1.0 - (avg_lat - LAT_SOFT_MS) / 60_000.0 * 0.3).max(LAT_PENALTY_FLOOR);
        }
        if self.consec_failures > STREAK_FREE {
            f *= 0.5_f64.powi((self.consec_failures - STREAK_FREE).min(3) as i32);
        }
        f
    }

    /// 加权几何池化：搜索 → 目录 → 正文是**串联**链路，任一环失效即不可交付
    fn pool(&self, use_lo: bool) -> Option<f64> {
        let chs = self.channels();
        if chs.is_empty() {
            return None;
        }
        let wsum: f64 = chs.iter().map(|c| c.0).sum();
        let acc: f64 = chs
            .iter()
            .map(|c| {
                let v = if use_lo { c.2 } else { c.1 };
                c.0 * v.max(POOL_FLOOR).ln()
            })
            .sum();
        Some(((acc / wsum).exp() * self.penalties()).clamp(0.0, 1.0))
    }

    /// 置信度点估计 0..1：该源下一次被调用时能交付可用内容的概率。
    /// `None` = 搜索/目录/正文三条通道都没有证据（旧版这里返回 0.5，把 947 行有目录证据的源
    /// 一起钉成"中性"）。
    pub fn confidence(&self) -> Option<f64> {
        self.pool(false)
    }

    /// 置信度下界（Wilson 95%）：**排序用它**，1/1 的源不会因为一次成功就冲到队首。
    pub fn confidence_lo(&self) -> Option<f64> {
        self.pool(true)
    }

    /// 有效样本：三通道尝试数之和。门限判断用它——旧版用 `attempts`（只记搜索），
    /// 导致只被取过目录的源永远进不了「≥5 次才算有样本」。
    pub fn effective_samples(&self) -> i64 {
        self.attempts + self.toc_attempts + self.content_attempts
    }

    /// 判死（与打分分离）：样本够、24h 内真失败过、剔除超时后成功率 <20%。
    /// 旧版用 `confidence() <= 0.05` 当门限，把"分数档"和"开关"绑在一起。
    pub fn is_dead(&self, now_ms: i64) -> bool {
        let decisive = (self.attempts - self.timeouts).max(0);
        decisive >= DEAD_MIN_SAMPLES
            && self.last_failure_at > 0
            && now_ms - self.last_failure_at <= DEAD_WINDOW_MS
            && (self.successes as f64 / decisive as f64) < DEAD_MAX_RATE
    }

    /// 搜索成功率（展示用，分母含超时——与 `confidence()` 的口径不同，别混用）
    pub fn success_rate(&self) -> f64 {
        if self.attempts == 0 {
            return 0.0;
        }
        self.successes as f64 / self.attempts as f64
    }

    pub fn avg_latency_ms(&self) -> i64 {
        if self.attempts == 0 {
            return 0;
        }
        self.lat_sum_ms / self.attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造统计行：lat_ms = 每次尝试的平均延迟
    fn stat(
        attempts: i64,
        successes: i64,
        timeouts: i64,
        consec_failures: i64,
        lat_ms: i64,
    ) -> SourceStat {
        SourceStat {
            user_namespace: "u".into(),
            source_url: "s".into(),
            attempts,
            successes,
            timeouts,
            lat_sum_ms: lat_ms * attempts,
            intro_hits: 0,
            intro_count: 0,
            toc_attempts: 0,
            toc_successes: 0,
            content_attempts: 0,
            content_successes: 0,
            consec_failures,
            last_failure_at: 0,
            updated_at: 0,
        }
    }

    /// 带目录/正文证据的行
    fn stat3(att: i64, suc: i64, toc: (i64, i64), content: (i64, i64), lat_ms: i64) -> SourceStat {
        let mut st = stat(att, suc, 0, 0, lat_ms);
        st.toc_attempts = toc.0;
        st.toc_successes = toc.1;
        st.content_attempts = content.0;
        st.content_successes = content.1;
        st
    }

    /// ① 没有证据就是 None —— 不再伪装成中性 0.5
    #[test]
    fn no_evidence_is_none() {
        assert_eq!(stat(0, 0, 0, 0, 0).confidence(), None);
        assert_eq!(stat(0, 0, 0, 0, 0).confidence_lo(), None);
    }

    /// ② 只有目录证据的源（生产里 947 行）也能出分，且差的目录给低分
    #[test]
    fn toc_only_uses_toc_evidence() {
        // 目录 0/52 全失败（生产 m.zhizihuan.com 的形态，搜索另算）
        let bad = stat3(0, 0, (52, 0), (0, 0), 3_000);
        let c = bad.confidence().expect("有目录证据就该出分");
        assert!(c < 0.1, "目录 0/52 必须低分，实际 {c}");
        // 目录 6/6 —— 小样本不该拿满分
        let tiny = stat3(0, 0, (6, 6), (0, 0), 3_000);
        let c = tiny.confidence().unwrap();
        assert!(
            (0.3..0.95).contains(&c),
            "6/6 的后验均值不该是 1.0，实际 {c}"
        );
        assert!(
            tiny.confidence_lo().unwrap() < 0.7,
            "6/6 的 Wilson 下界应明显低于点估计"
        );
    }

    /// ③ 搜索笑嘻嘻、目录哭哈哈 → 低分；三通道都好 → 高分（旧版这里是倒挂）
    #[test]
    fn contradicted_source_scores_below_healthy_source() {
        let contradicted = stat3(92, 91, (52, 0), (0, 0), 3_000);
        let healthy = stat3(92, 91, (44, 40), (22, 20), 3_000);
        let (c1, c2) = (
            contradicted.confidence().unwrap(),
            healthy.confidence().unwrap(),
        );
        assert!(c1 < 0.3, "矛盾源应低分，实际 {c1}");
        assert!(c2 > 0.6, "三通道都好应高分，实际 {c2}");
        assert!(c1 + 0.3 < c2, "矛盾源必须显著低于好源：{c1} vs {c2}");
    }

    /// ④ 小样本不冲顶：1/1 的下界远低于 100/100
    #[test]
    fn tiny_sample_does_not_top_the_ranking() {
        let tiny = stat(1, 1, 0, 0, 500).confidence_lo().unwrap();
        let solid = stat(100, 100, 0, 0, 500).confidence_lo().unwrap();
        assert!(tiny < 0.4, "1/1 的排序键必须被压住，实际 {tiny}");
        assert!(solid > 0.9, "100/100 的下界应接近 1，实际 {solid}");
    }

    /// ⑤ 连败是滞回不是断崖：成功率 0.83 的源连败 3 次只掉一半，不归零
    #[test]
    fn streak_is_hysteresis_not_a_cliff() {
        let base = stat(93, 77, 0, 0, 1_000);
        let once = stat(93, 77, 0, 3, 1_000);
        let more = stat(93, 77, 0, 5, 1_000);
        let mid = stat(93, 77, 0, 4, 1_000);
        let (b, o, mid_c, m) = (
            base.confidence().unwrap(),
            once.confidence().unwrap(),
            mid.confidence().unwrap(),
            more.confidence().unwrap(),
        );
        assert!(b > 0.7, "成功 77/93 的源基线应高分，实际 {b}");
        assert!((o / b - 0.5).abs() < 1e-9, "连败 3 次应恰好减半：{o} / {b}");
        assert!(
            (mid_c / b - 0.25).abs() < 1e-9,
            "连败 4 次应剩 1/4：{mid_c} / {b}"
        );
        assert!((m / b - 0.125).abs() < 1e-9, "连败 5 次封顶 1/8：{m} / {b}");
        assert!(o > 0.05, "连败 3 次不得被一刀压到死源档，实际 {o}");
    }

    /// ⑥ 慢是"慢"不是"坏"：30s 只温和扣分（≥0.7 倍），不再整体压到 0.3 倍
    #[test]
    fn slow_source_gets_mild_penalty_only() {
        let fast = stat(20, 18, 0, 0, 1_000).confidence().unwrap();
        let slow = stat(20, 18, 0, 0, 30_000).confidence().unwrap();
        assert!(
            slow / fast >= 0.69,
            "慢源惩罚不该超过 30%：{} vs {fast}",
            slow
        );
        assert!(slow < fast, "慢源仍应略低");
    }

    /// ⑦ 纯超时的好源不判死；真连败的坏源判死；样本不足不判死
    #[test]
    fn dead_judgement_needs_samples_and_real_failures() {
        let now = 1_000_000_000_000i64;
        // 60 次里 50 次超时、8 次成功 → 剔除超时后 8/10 成功，不是死源
        let mut timeout_heavy = stat(60, 8, 50, 0, 25_000);
        timeout_heavy.last_failure_at = now - 60_000;
        assert!(!timeout_heavy.is_dead(now), "纯超时的好源不得判死");
        assert!(timeout_heavy.confidence().unwrap() > 0.05, "也不该贴死源档");

        // 20 次全失败、刚失败过 → 死源
        let mut bad = stat(20, 0, 0, 5, 1_000);
        bad.last_failure_at = now - 60_000;
        assert!(bad.is_dead(now), "成功率 0/20 且刚失败必须判死");

        // 只有 3 次失败 → 样本不足，不判死
        let mut few = stat(3, 0, 0, 3, 1_000);
        few.last_failure_at = now - 60_000;
        assert!(!few.is_dead(now), "有效样本 <5 不该判死");

        // 失败是 30 小时前的事 → 窗口外，放行
        let mut stale = stat(20, 0, 0, 5, 1_000);
        stale.last_failure_at = now - 30 * 3_600_000;
        assert!(!stale.is_dead(now), "超过 24h 没再失败应放行");
    }

    /// ⑧ 有效样本 = 三通道之和（只被取过目录的源也能进"有样本"的门限）
    #[test]
    fn effective_samples_counts_all_channels() {
        let st = stat3(0, 0, (12, 10), (4, 4), 1_000);
        assert_eq!(st.effective_samples(), 16);
        assert_eq!(stat(7, 5, 0, 0, 900).effective_samples(), 7);
    }
}
