//! 按键 HUD 卡片栈（平台无关纯逻辑，macOS / Windows 共用）。
//!
//! 连续按键时每一步各占一张卡片：新卡片出现在底部槽位，已有卡片平滑
//! 向上挤，每张卡片按自己的出现时间独立计时淡出（同一组键连按合并为
//! ×N 计数），避免单个 HUD 反复重绘造成的快速闪烁。偏移以「屏幕向下为正」
//! 表示（最新卡片为 0，越早越靠上为负），AppKit 坐标系由调用方翻转。
use std::time::{Duration, Instant};

/// 每张卡片完全显示时长（自出现起计），之后开始淡出。
pub(crate) const HOLD: Duration = Duration::from_millis(1200);
/// 每帧（≈16ms）淡出步长。
const FADE_STEP: f64 = 0.08;
/// 每帧淡入步长（新卡片约 4 帧完全显现）。
const FADE_IN_STEP: f64 = 0.25;
/// 超出栈容量被挤出的卡片加速淡出步长。
const EVICT_FADE_STEP: f64 = 0.2;
/// 同时显示的卡片上限；超出时最早的卡片加速淡出。
pub(crate) const MAX_CARDS: usize = 5;
/// 卡片之间的竖直间距（基准单位：Windows 96 DPI 像素 / macOS points）。
const STACK_GAP: f64 = 10.0;
/// 上挤动画：每帧向目标位置指数趋近的比例。
const SLIDE_EASE: f64 = 0.3;
/// 新卡片从槽位下方该距离滑入。
pub(crate) const ENTER_OFFSET: f64 = 16.0;

/// 卡片生命周期阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// 淡入/保持中，到期前不透明。
    Visible,
    /// 正常到期淡出。
    Fading,
    /// 被挤出栈，加速淡出。
    Evicted,
}

/// 栈中一张卡片的状态（不含窗口资源）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Card {
    pub id: u64,
    pub keys: Vec<String>,
    pub repeat: u32,
    pub height: f64,
    /// 到期时间：出现（或合并连按）时刻 + HOLD。
    pub hold_until: Instant,
    pub alpha: f64,
    /// 当前竖直偏移（相对底部槽位顶边，向上为负）。
    pub offset: f64,
    pub phase: Phase,
}

/// `push` 的结果：新建卡片或合并到最新卡片（需要重绘 ×N）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pushed {
    New(u64),
    Merged(u64),
}

/// 卡片栈：`cards` 按出现先后排列（末尾最新，位于底部槽位）。
#[derive(Debug, Default)]
pub(crate) struct Stack {
    pub cards: Vec<Card>,
    next_id: u64,
    gap: f64,
    enter_offset: f64,
}

impl Stack {
    pub fn new(scale: f64) -> Self {
        Self {
            cards: Vec::new(),
            next_id: 0,
            gap: STACK_GAP * scale,
            enter_offset: ENTER_OFFSET * scale,
        }
    }

    /// 压入一组按键。与最新卡片相同且其尚未开始淡出时合并计数并重置其计时；
    /// 否则新建卡片置于底部，超出容量的最早卡片转为加速淡出。
    pub fn push(&mut self, keys: &[String], height: f64, now: Instant) -> Pushed {
        if let Some(last) = self.cards.last_mut()
            && last.phase == Phase::Visible
            && last.keys == keys
        {
            last.repeat += 1;
            last.hold_until = now + HOLD;
            last.alpha = 1.0;
            return Pushed::Merged(last.id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.cards.push(Card {
            id,
            keys: keys.to_vec(),
            repeat: 1,
            height,
            hold_until: now + HOLD,
            alpha: 0.0,
            offset: self.enter_offset,
            phase: Phase::Visible,
        });
        let alive = self
            .cards
            .iter()
            .filter(|c| c.phase != Phase::Evicted)
            .count();
        if alive > MAX_CARDS {
            let overflow = alive - MAX_CARDS;
            for card in self
                .cards
                .iter_mut()
                .filter(|c| c.phase != Phase::Evicted)
                .take(overflow)
            {
                card.phase = Phase::Evicted;
            }
        }
        Pushed::New(id)
    }

    /// 各卡片的目标偏移：最新卡片为 0，越早的卡片越靠上（负值）。
    pub fn targets(&self) -> Vec<f64> {
        let mut targets = vec![0.0; self.cards.len()];
        let mut y = 0.0;
        for (i, card) in self.cards.iter().enumerate().rev() {
            if i + 1 < self.cards.len() {
                y -= card.height + self.gap;
            }
            targets[i] = y;
        }
        targets
    }

    /// 推进一帧：淡入、到期淡出、上挤滑动；返回本帧被移除的卡片 id。
    pub fn advance(&mut self, now: Instant) -> Vec<u64> {
        let targets = self.targets();
        for (card, target) in self.cards.iter_mut().zip(targets) {
            if card.phase == Phase::Visible && now >= card.hold_until {
                card.phase = Phase::Fading;
            }
            card.alpha = match card.phase {
                Phase::Visible => (card.alpha + FADE_IN_STEP).min(1.0),
                Phase::Fading => (card.alpha - FADE_STEP).max(0.0),
                Phase::Evicted => (card.alpha - EVICT_FADE_STEP).max(0.0),
            };
            let next = card.offset + (target - card.offset) * SLIDE_EASE;
            card.offset = if (next - target).abs() < 0.5 {
                target
            } else {
                next
            };
        }
        let mut removed = Vec::new();
        self.cards.retain(|card| {
            let done = card.phase != Phase::Visible && card.alpha <= 0.0;
            if done {
                removed.push(card.id);
            }
            !done
        });
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn stack_pushes_new_cards_to_bottom_and_older_upwards() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        stack.push(&keys(&["Ctrl", "A"]), 64.0, now);
        stack.push(&keys(&["Ctrl", "C"]), 64.0, now);
        stack.push(&keys(&["End"]), 64.0, now);
        assert_eq!(stack.targets(), vec![-148.0, -74.0, 0.0]);
    }

    #[test]
    fn stack_merges_repeated_keys_into_newest_card() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        assert_eq!(stack.push(&keys(&["Enter"]), 64.0, now), Pushed::New(0));
        assert_eq!(
            stack.push(&keys(&["Enter"]), 64.0, now + Duration::from_millis(300)),
            Pushed::Merged(0)
        );
        assert_eq!(stack.cards.len(), 1);
        assert_eq!(stack.cards[0].repeat, 2);
        assert_eq!(
            stack.cards[0].hold_until,
            now + Duration::from_millis(300) + HOLD
        );
        // 不同键 → 新卡片；再按 Enter 不会合并到旧卡片。
        assert_eq!(stack.push(&keys(&["Tab"]), 64.0, now), Pushed::New(1));
        assert_eq!(stack.push(&keys(&["Enter"]), 64.0, now), Pushed::New(2));
    }

    #[test]
    fn cards_expire_independently_by_their_own_appear_time() {
        let t0 = Instant::now();
        let mut stack = Stack::new(1.0);
        stack.push(&keys(&["A"]), 64.0, t0);
        stack.push(&keys(&["B"]), 64.0, t0 + Duration::from_millis(500));
        for _ in 0..5 {
            stack.advance(t0 + Duration::from_millis(600));
        }
        // 第一张到期，第二张仍在保持期。
        let t1 = t0 + HOLD + Duration::from_millis(10);
        stack.advance(t1);
        assert_eq!(stack.cards[0].phase, Phase::Fading);
        assert_eq!(stack.cards[1].phase, Phase::Visible);
        let mut removed = Vec::new();
        for _ in 0..30 {
            removed.extend(stack.advance(t1));
        }
        assert_eq!(removed, vec![0]);
        assert_eq!(stack.cards.len(), 1);
        assert_eq!(stack.cards[0].id, 1);
        assert_eq!(stack.cards[0].alpha, 1.0);
        assert_eq!(stack.targets(), vec![0.0]);
    }

    #[test]
    fn overflow_evicts_oldest_cards() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        for i in 0..(MAX_CARDS + 2) {
            stack.push(&[format!("F{i}")], 64.0, now);
        }
        let evicted: Vec<u64> = stack
            .cards
            .iter()
            .filter(|c| c.phase == Phase::Evicted)
            .map(|c| c.id)
            .collect();
        assert_eq!(evicted, vec![0, 1]);
        let mut removed = Vec::new();
        for _ in 0..10 {
            removed.extend(stack.advance(now));
        }
        assert_eq!(removed, vec![0, 1]);
        assert_eq!(stack.cards.len(), MAX_CARDS);
    }

    #[test]
    fn new_card_slides_in_and_fades_in() {
        let now = Instant::now();
        let mut stack = Stack::new(1.0);
        stack.push(&keys(&["A"]), 64.0, now);
        assert_eq!(stack.cards[0].alpha, 0.0);
        assert_eq!(stack.cards[0].offset, ENTER_OFFSET);
        for _ in 0..20 {
            stack.advance(now);
        }
        assert_eq!(stack.cards[0].alpha, 1.0);
        assert_eq!(stack.cards[0].offset, 0.0);
    }
}
