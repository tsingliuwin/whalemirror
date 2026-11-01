//! composer 统计弹层的开合状态机（上游 `ui-chat` `useStatDialog` 的语义）。
//!
//! 上游每个统计 pill 是一个 `DialogPill`：`aria-expanded` 表示打开，
//! `useStatDialog()` 负责「点开、点同一个 pill 关、Escape 关、点面板外关」，
//! 并保证整条 dock 上**同时最多一个**弹层。rustdsh 此前只有「点同一个 pill
//! 开合」，Esc 与点外都不会关，键盘用户被卡在弹层里。
//!
//! 状态在这里写成纯函数（不依赖 GPUI），便于单测锁定语义；UI 层只负责
//! 把点击/按键/位置事件映射成 `StatPopupInput`。

/// 统计弹层种类（与 `dsh-gpui` bin 里的 `StatDialogKind` 一一对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatPopup {
    /// 会话统计（模型用时 / 工具用时 / TTFT / TPS）。
    Gauge,
    /// Token 用量（总量 / 缓存命中 / 四桶明细）。
    Usage,
}

/// 一次可以作用到弹层上的输入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatPopupInput {
    /// 点了某个 pill（`Some(kind)` 即点的是那颗 pill）。
    Toggle(StatPopup),
    /// 按了 Escape。
    Escape,
    /// 在面板与 pill 之外按下了鼠标。
    PressOutside,
    /// 点在了弹层或它所属 pill 的区域内（或其它不相关的点击）。
    PressInside,
}

/// 计算输入作用后的新弹层状态。
///
/// 语义（对齐上游 `useStatDialog`）：
/// - 点同一颗 pill → 关；点另一颗 → 换成那颗（互斥，不会同时开两个）；
/// - Escape → 关；
/// - 点面板外 → 关；点面板内或 pill 上 → 不变（pill 自己的点击另有 `Toggle`）；
/// - 弹层未开时，除 `Toggle` 外的输入都是空操作（不凭空开）。
pub fn stat_popup_next(current: Option<StatPopup>, input: StatPopupInput) -> Option<StatPopup> {
    match input {
        StatPopupInput::Toggle(kind) => {
            if current == Some(kind) {
                None
            } else {
                Some(kind)
            }
        }
        StatPopupInput::Escape | StatPopupInput::PressOutside => None,
        StatPopupInput::PressInside => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use StatPopup::{Gauge, Usage};
    use StatPopupInput::{Escape, PressInside, PressOutside, Toggle};

    #[test]
    fn clicking_the_same_pill_closes_and_the_other_switches() {
        assert_eq!(stat_popup_next(None, Toggle(Gauge)), Some(Gauge));
        // 同一颗再点一次 → 关（上游 pill 的 aria-expanded 取反）
        assert_eq!(stat_popup_next(Some(Gauge), Toggle(Gauge)), None);
        // 另一颗 → 直接换成它，绝不出现两个同时打开
        assert_eq!(stat_popup_next(Some(Gauge), Toggle(Usage)), Some(Usage));
        assert_eq!(stat_popup_next(Some(Usage), Toggle(Gauge)), Some(Gauge));
    }

    #[test]
    fn escape_closes_an_open_popup_and_is_a_noop_when_closed() {
        assert_eq!(stat_popup_next(Some(Gauge), Escape), None);
        assert_eq!(stat_popup_next(Some(Usage), Escape), None);
        // 未开时按 Esc 不应凭空开出弹层
        assert_eq!(stat_popup_next(None, Escape), None);
    }

    #[test]
    fn outside_press_closes_but_inside_press_keeps_it() {
        assert_eq!(stat_popup_next(Some(Usage), PressOutside), None);
        assert_eq!(stat_popup_next(Some(Gauge), PressOutside), None);
        // 点面板内（含点数字文本）不该关 —— 用户可能只是想选中文字
        assert_eq!(stat_popup_next(Some(Usage), PressInside), Some(Usage));
        assert_eq!(stat_popup_next(None, PressInside), None);
        assert_eq!(stat_popup_next(None, PressOutside), None);
    }

    #[test]
    fn at_most_one_popup_is_ever_open() {
        // 任意输入序列后，状态只会是 None 或单个种类（类型即保证，但锁一下转换图）
        let kinds = [Gauge, Usage];
        let mut state = None;
        for k in kinds {
            state = stat_popup_next(state, Toggle(k));
            assert!(matches!(state, Some(Gauge) | Some(Usage)));
            state = stat_popup_next(state, Escape);
            assert_eq!(state, None);
        }
    }
}