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

impl SourceStat {
    /// 置信度 0..1: 质量分(成功率 .55 + 目录 .15 + 正文 .15 + 丰富度 .15) × 速度因子.
    /// 速度因子 = 0.3 + 0.7×(1 - 平均延迟/30s): 慢源(平均 >15s)乘法压到冷启动 0.5 之下,
    /// 排队尾不挡首批结果; 冷启动 0.5; 连败>=3 或 尝试>=5 且成功率<0.2 → 压到 0.05 (排队尾, 不删除)
    ///
    /// **成功率分母剔除超时**（`attempts - timeouts`）：超时是「慢」不是「坏」——上一轮已把
    /// 超时从 `consec_failures` 摘出，但只要它仍占分母，一个只是经常慢到超时的好源照样会被
    /// 「尝试>=5 且成功率<0.2」压到 0.05，再被 6h 死源窗口滤掉。单源端到端 deadline 上线后
    /// 超时必然变多，分母必须同步剔除。`timeouts` 为 0 的老数据行为完全不变（= 原分母）。
    pub fn confidence(&self) -> f64 {
        if self.attempts == 0 {
            return 0.5;
        }
        // 有效尝试 = 真正给出成败结论的尝试（剔除纯超时）；全超时/负值 → 无证据，取中性 0.5
        let decisive = (self.attempts - self.timeouts).max(0) as f64;
        let rel = if decisive > 0.0 {
            self.successes as f64 / decisive
        } else {
            0.5
        };
        let toc_rel = if self.toc_attempts > 0 {
            self.toc_successes as f64 / self.toc_attempts as f64
        } else {
            rel
        };
        let cont_rel = if self.content_attempts > 0 {
            self.content_successes as f64 / self.content_attempts as f64
        } else {
            toc_rel
        };
        let avg_lat = self.lat_sum_ms as f64 / self.attempts as f64;
        let speed = (1.0 - (avg_lat / 30000.0).min(1.0)).max(0.0);
        let rich = if self.intro_count > 0 {
            self.intro_hits as f64 / self.intro_count as f64
        } else {
            0.5
        };
        let quality = 0.55 * rel + 0.15 * toc_rel + 0.15 * cont_rel + 0.15 * rich;
        let mut c = quality * (0.3 + 0.7 * speed);
        if self.consec_failures >= 3 || (self.attempts >= 5 && rel < 0.2) {
            c = c.min(0.05);
        }
        c.clamp(0.0, 1.0)
    }

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

    /// 造统计行：lat_ms = 每次尝试的平均延迟（简化构造，只影响速度因子）
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
            intro_hits: successes,
            intro_count: attempts.max(1),
            toc_attempts: 0,
            toc_successes: 0,
            content_attempts: 0,
            content_successes: 0,
            consec_failures,
            last_failure_at: 0,
            updated_at: 0,
        }
    }

    /// 改造前的公式（成功率分母含超时）——仅用于向后兼容对照
    fn legacy_confidence(s: &SourceStat) -> f64 {
        if s.attempts == 0 {
            return 0.5;
        }
        let rel = s.successes as f64 / s.attempts as f64;
        let toc_rel = if s.toc_attempts > 0 {
            s.toc_successes as f64 / s.toc_attempts as f64
        } else {
            rel
        };
        let cont_rel = if s.content_attempts > 0 {
            s.content_successes as f64 / s.content_attempts as f64
        } else {
            toc_rel
        };
        let avg_lat = s.lat_sum_ms as f64 / s.attempts as f64;
        let speed = (1.0 - (avg_lat / 30000.0).min(1.0)).max(0.0);
        let rich = if s.intro_count > 0 {
            s.intro_hits as f64 / s.intro_count as f64
        } else {
            0.5
        };
        let quality = 0.55 * rel + 0.15 * toc_rel + 0.15 * cont_rel + 0.15 * rich;
        let mut c = quality * (0.3 + 0.7 * speed);
        if s.consec_failures >= 3 || (s.attempts >= 5 && rel < 0.2) {
            c = c.min(0.05);
        }
        c.clamp(0.0, 1.0)
    }

    /// ① 纯超时的好源：成功率低只因超时，不得被判死（改造前 rel=8/60=0.13 → 压到 0.05）
    #[test]
    fn timeout_heavy_good_source_not_judged_dead() {
        // 60 次尝试里 50 次超时、8 次成功、2 次真失败，连败 0：deadline 上线后最典型的形态
        let st = stat(60, 8, 50, 0, 25_000);
        assert!(
            legacy_confidence(&st) <= 0.05,
            "前置：改造前这种源确实会被判死（{}）",
            legacy_confidence(&st)
        );
        let c = st.confidence();
        assert!(c > 0.1, "纯超时的好源不得被判死，实际 {c}");
        // 也该恢复成「可参与排序」的档位，而不是贴着 0.05
        assert!(c > st.confidence().min(0.05) + 0.05);
    }

    /// ①b 全部尝试都超时（successes=0/timeouts=attempts）：无成败证据，不判死
    #[test]
    fn all_timeouts_is_undecided_not_dead() {
        let st = stat(12, 0, 12, 0, 30_000);
        assert!(st.confidence() > 0.05, "全超时无证据不应判死");
    }

    /// ② 真连败的坏源：仍被判死（阈值不动）
    #[test]
    fn real_bad_source_still_judged_dead() {
        // 2a. 连败 >= 3
        let st = stat(20, 0, 0, 5, 1_000);
        assert!(st.confidence() <= 0.05, "连败 5 次必须仍判死");
        // 2b. 无连败但成功率极低（真失败，不是超时）
        let st = stat(20, 1, 0, 0, 1_000);
        assert!(st.confidence() <= 0.05, "成功率 1/20 且无超时必须仍判死");
        // 2c. 有超时但剔除后仍是真失败占多数（18 超时 + 2 次真失败 + 0 成功）
        let st = stat(20, 0, 18, 0, 20_000);
        assert!(st.confidence() <= 0.05, "剔除超时后成功率 0/2 必须仍判死");
    }

    /// ③ 向后兼容：timeouts=0 的老行与新公式逐位一致
    #[test]
    fn rows_without_timeouts_unchanged() {
        for &(att, suc, consec, lat) in &[
            (10, 5, 0, 800),
            (5, 0, 0, 800),
            (40, 39, 0, 300),
            (7, 1, 2, 20_000),
            (1, 1, 0, 60_000),
            (3, 0, 3, 5_000),
        ] {
            let st = stat(att, suc, 0, consec, lat);
            let (new, old) = (st.confidence(), legacy_confidence(&st));
            assert!(
                (new - old).abs() < 1e-12,
                "timeouts=0 时应与旧公式一致：attempts={att} successes={suc} consec={consec} new={new} old={old}"
            );
        }
    }

    /// attempts=0 仍是冷启动 0.5
    #[test]
    fn cold_start_unchanged() {
        assert_eq!(stat(0, 0, 0, 0, 0).confidence(), 0.5);
    }
}
