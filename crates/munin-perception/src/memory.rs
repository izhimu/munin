use anyhow::{Context, Result};
use munin_types::MemoryEntry;
use std::path::PathBuf;

/// 对比式恢复记忆库 (RecoveryMemory)
///
/// 存储历史故障的正反双向描述与有效恢复动作，
/// 通过快引擎批量 noul 探针实现语义级相似故障匹配。
/// 持久化至 `~/.munin/recovery.json`，跨会话累积学习。
#[derive(Debug, Clone, Default)]
pub struct RecoveryMemory {
    pub entries: Vec<MemoryEntry>,
    /// 记忆容量上限（超出按 LRU 淘汰最旧低频条目）
    pub max_entries: usize,
    /// 持久化路径（None 时仅内存）
    persist_path: Option<PathBuf>,
}

impl RecoveryMemory {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 100,
            persist_path: Self::default_path(),
        }
    }

    /// 仅内存实例（测试用）
    pub fn in_memory() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 100,
            persist_path: None,
        }
    }

    fn default_path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".munin").join("recovery.json"))
    }

    /// 从磁盘加载已有记忆（不存在时为空库）
    pub fn load(&mut self) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read recovery memory at {}", path.display()))?;
        self.entries = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse recovery memory at {}", path.display()))?;
        Ok(())
    }

    /// 持久化至磁盘
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.entries)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// 记录一次恢复结果：成功则新增或强化条目，失败则标记
    pub fn record(
        &mut self,
        applicable_desc: impl Into<String>,
        inapplicable_desc: impl Into<String>,
        recovery: munin_types::RecoveryKind,
        target: impl Into<String>,
        succeeded: bool,
    ) {
        let app_desc = applicable_desc.into();
        let target = target.into();

        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.applicable_desc == app_desc && e.target == target)
        {
            if succeeded {
                entry.success_count += 1;
            } else {
                entry.failure_count += 1;
            }
            return;
        }

        if succeeded {
            self.entries.push(MemoryEntry {
                applicable_desc: app_desc,
                inapplicable_desc: inapplicable_desc.into(),
                recovery,
                target,
                success_count: 1,
                failure_count: 0,
            });
            self.evict_if_needed();
        }
    }

    /// 标记指定索引条目本次恢复失败
    pub fn mark_failure(&mut self, idx: usize) {
        if let Some(entry) = self.entries.get_mut(idx) {
            entry.failure_count += 1;
        }
    }

    /// 标记指定索引条目本次恢复成功
    pub fn mark_success(&mut self, idx: usize) {
        if let Some(entry) = self.entries.get_mut(idx) {
            entry.success_count += 1;
        }
    }

    /// 超出容量时淘汰成功率最低的条目；同分淘汰最旧（Vec 顺序即插入顺序）
    fn evict_if_needed(&mut self) {
        while self.entries.len() > self.max_entries {
            // ponytail: O(n) 扫描找最差条目，n ≤ 100 可接受；条目膨胀时改堆
            // min_by 平局取靠前者 = 最旧条目，符合 LRU 语义
            let worst_idx = self
                .entries
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    a.success_rate()
                        .partial_cmp(&b.success_rate())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i);
            if let Some(idx) = worst_idx {
                self.entries.remove(idx);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munin_types::RecoveryKind;

    #[test]
    fn test_memory_record_and_rate() {
        let mut mem = RecoveryMemory::in_memory();
        mem.record(
            "当页面出现模态遮罩时",
            "当页面无遮罩时",
            RecoveryKind::Dismiss,
            "btn-close",
            true,
        );
        assert_eq!(mem.entries.len(), 1);
        assert_eq!(mem.entries[0].success_rate(), 1.0);

        mem.record(
            "当页面出现模态遮罩时",
            "当页面无遮罩时",
            RecoveryKind::Dismiss,
            "btn-close",
            false,
        );
        assert_eq!(mem.entries.len(), 1);
        assert_eq!(mem.entries[0].success_count, 1);
        assert_eq!(mem.entries[0].failure_count, 1);
        assert_eq!(mem.entries[0].success_rate(), 0.5);
    }

    #[test]
    fn test_memory_eviction() {
        let mut mem = RecoveryMemory::in_memory();
        mem.max_entries = 3;
        for i in 0..5 {
            mem.record(
                format!("故障场景 {}", i),
                "边界",
                RecoveryKind::Retry,
                "page",
                true,
            );
        }
        assert_eq!(mem.entries.len(), 3);
    }

    #[test]
    fn test_memory_persistence_roundtrip() {
        let dir = std::env::temp_dir().join(format!("munin-mem-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recovery.json");

        let mut mem = RecoveryMemory::in_memory();
        mem.persist_path = Some(path.clone());
        mem.record("场景A", "边界A", RecoveryKind::Refresh, "page", true);
        mem.save().unwrap();

        let mut mem2 = RecoveryMemory::in_memory();
        mem2.persist_path = Some(path.clone());
        mem2.load().unwrap();
        assert_eq!(mem2.entries.len(), 1);
        assert_eq!(mem2.entries[0].applicable_desc, "场景A");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
