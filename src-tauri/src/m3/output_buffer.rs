//! 内存环形缓冲：序号单调、时间戳由后端生成（§3.6 OutputLine）。

use std::collections::VecDeque;

use serde::Serialize;

/// 一行运行输出。内存缓冲、日志文件与 `instance:output` 事件用的是同一份数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputLine {
    pub seq: u64,
    pub timestamp_ms: i64,
    pub text: String,
    pub is_stderr: bool,
}

/// 环形缓冲。容量来自设置项 `outputBufferLines`（§3.7）。
#[derive(Debug)]
pub struct OutputBuffer {
    capacity: usize,
    lines: VecDeque<OutputLine>,
    next_seq: u64,
    /// 已被挤出的最小序号：`fromSeq` 小于它就说明客户端缺了行。
    dropped_before: u64,
}

impl OutputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            lines: VecDeque::new(),
            next_seq: 0,
            dropped_before: 0,
        }
    }

    /// 追加一行并返回它的序号。序号从 0 起、单调递增、不重复。
    pub fn push(&mut self, timestamp_ms: i64, text: String, is_stderr: bool) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.lines.push_back(OutputLine {
            seq,
            timestamp_ms,
            text,
            is_stderr,
        });
        while self.lines.len() > self.capacity {
            if let Some(evicted) = self.lines.pop_front() {
                self.dropped_before = evicted.seq + 1;
            }
        }
        seq
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// 客户端已持有 `from_seq - 1` 为止的行；返回 `from_seq` 起的全部行。
    ///
    /// 返回 `(lines, next_seq, truncated)`。`truncated` 为真表示客户端要的起点已经被挤出，
    /// 此时返回的是**当前缓冲的全部内容**，前端必须先清空再重建（否则会缺行）。
    pub fn page(&self, from_seq: u64) -> (Vec<OutputLine>, u64, bool) {
        let truncated = from_seq < self.dropped_before;
        let lines = self
            .lines
            .iter()
            .filter(|l| l.seq >= from_seq)
            .cloned()
            .collect();
        (lines, self.next_seq, truncated)
    }

    /// 全部内容（导出用）。
    pub fn snapshot(&self) -> Vec<OutputLine> {
        self.lines.iter().cloned().collect()
    }

    /// 清空行但**不**重置序号。
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_is_monotonic_from_zero_and_keeps_stderr_flag() {
        let mut b = OutputBuffer::new(10);
        assert_eq!(b.push(1, "a".to_string(), false), 0);
        assert_eq!(b.push(2, "b".to_string(), true), 1);
        assert_eq!(b.push(3, "c".to_string(), false), 2);

        let (lines, next, truncated) = b.page(0);
        assert_eq!(lines.len(), 3);
        assert_eq!(next, 3);
        assert!(!truncated);
        assert!(!lines[0].is_stderr);
        assert!(lines[1].is_stderr);
        assert_eq!(lines[2].timestamp_ms, 3);
    }

    #[test]
    fn overflow_evicts_oldest_and_reports_truncated() {
        let mut b = OutputBuffer::new(3);
        for i in 0..5 {
            b.push(i as i64, format!("line-{i}"), false);
        }
        assert_eq!(b.len(), 3, "容量必须来自设置项 outputBufferLines");

        let (lines, next, truncated) = b.page(0);
        assert!(truncated, "起点已被挤出缓冲时必须报 truncated");
        assert_eq!(
            lines.iter().map(|l| l.seq).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(next, 5);

        let (from_evicted, _, truncated2) = b.page(2);
        assert!(!truncated2, "起点正好还在缓冲里，不算被截断");
        assert_eq!(from_evicted.first().unwrap().seq, 2);
    }

    #[test]
    fn paging_from_the_future_returns_empty_without_truncation() {
        let mut b = OutputBuffer::new(3);
        b.push(1, "a".to_string(), false);
        let (lines, next, truncated) = b.page(99);
        assert!(lines.is_empty());
        assert_eq!(next, 1);
        assert!(!truncated);
    }

    #[test]
    fn clear_keeps_the_sequence_monotonic() {
        let mut b = OutputBuffer::new(3);
        b.push(1, "a".to_string(), false);
        b.clear();
        assert!(b.is_empty());
        assert_eq!(
            b.push(2, "b".to_string(), false),
            1,
            "清空不得让序号回退，否则前端 fromSeq 会错乱"
        );
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let mut b = OutputBuffer::new(0);
        b.push(1, "a".to_string(), false);
        assert_eq!(b.len(), 1);
    }
}
