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
    pub fn confidence(&self) -> f64 {
        if self.attempts == 0 {
            return 0.5;
        }
        let rel = self.successes as f64 / self.attempts as f64;
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
