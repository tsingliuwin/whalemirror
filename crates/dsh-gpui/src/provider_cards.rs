//! 模型页提供方表单的互斥开合状态。
//!
//! 上游 `ModelsSection`：**同一时刻只开一张表单**——点行内「编辑」时
//! `setAddOpen(false)` 再切换 `editing`（同一张再点即收起）；点「添加提供方」
//! 时反过来收起行编辑卡。rustdsh 曾允许添加卡与行编辑卡同时展开两份草稿，
//! 切换时也不会收起另一张。状态写成纯函数便于单测锁定。

/// 提供方卡片的开合焦点：行编辑目标 + 添加卡是否打开。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderFormFocus {
    /// 正在编辑的提供方 id；`None` = 没有行编辑卡。
    pub editing: Option<String>,
    /// 添加卡是否打开。
    pub add_open: bool,
}

/// 点行内「编辑」后：同一张再点收起，否则切到该张；添加卡一律收起。
pub fn focus_after_edit(current: &ProviderFormFocus, clicked: &str) -> ProviderFormFocus {
    ProviderFormFocus {
        editing: if current.editing.as_deref() == Some(clicked) {
            None
        } else {
            Some(clicked.to_string())
        },
        add_open: false,
    }
}

/// 点「添加提供方」后：打开添加卡，行编辑卡一律收起。
pub fn focus_after_add(current: &ProviderFormFocus) -> ProviderFormFocus {
    ProviderFormFocus {
        editing: None,
        add_open: !current.add_open,
    }
}

/// 关掉添加卡（取消/提交）：行编辑焦点不变。
pub fn focus_after_add_closed(current: &ProviderFormFocus) -> ProviderFormFocus {
    ProviderFormFocus { editing: current.editing.clone(), add_open: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn focus(editing: Option<&str>, add_open: bool) -> ProviderFormFocus {
        ProviderFormFocus { editing: editing.map(str::to_string), add_open }
    }

    #[test]
    fn editing_one_provider_closes_the_add_card() {
        let next = focus_after_edit(&focus(None, true), "zai");
        assert_eq!(next.editing.as_deref(), Some("zai"));
        assert!(!next.add_open);
    }

    #[test]
    fn clicking_the_same_row_edit_again_collapses_it() {
        let next = focus_after_edit(&focus(Some("zai"), false), "zai");
        assert_eq!(next.editing, None);
        assert!(!next.add_open);
    }

    #[test]
    fn editing_another_provider_switches_the_single_card() {
        let next = focus_after_edit(&focus(Some("zai"), false), "火山");
        assert_eq!(next.editing.as_deref(), Some("火山"));
    }

    #[test]
    fn opening_the_add_card_closes_the_row_editor() {
        let next = focus_after_add(&focus(Some("zai"), false));
        assert_eq!(next.editing, None);
        assert!(next.add_open);
        // 再点一次「添加」收起，仍不恢复行编辑
        let closed = focus_after_add(&next);
        assert_eq!(closed.editing, None);
        assert!(!closed.add_open);
        assert_eq!(focus_after_add_closed(&next), focus(None, false));
    }
}
