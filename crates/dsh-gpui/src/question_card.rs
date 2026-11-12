//! 提问卡纯状态（上游 `ui-user-questions` QuestionComposer 的 rustdsh 等价
//! 子集：分页单题 + 选项单/多选 + 自定义答案 + 跳过 + 推荐隐式预选）。
//!
//! 渲染与输入框实体在宿主（main.rs 遮罩卡），这里只持可测的判定面。

use dsh_tools::{AskUserAnswerItem, AskUserQuestion};

/// UI → hook 的应答（通道消息；hook 轮询收）。
#[derive(Clone, Debug, PartialEq)]
pub enum QuestionUiAnswer {
    /// 提交完整答案批（每题恰一项）。
    Submit(Vec<AskUserAnswerItem>),
    /// 放弃整组问题（X 钮 / 轮次中止——hook 回 Cancelled，工具结果
    /// ASK_ABORTED 文案回模型）。
    Cancel,
}

/// 卡头等待态文案（上游 `wait.held`，zh 逐字；legacy 阻塞无倒计时恒此态）。
pub const WAIT_HELD_LABEL: &str = "会一直等你回答";

/// 未完成校验文案（上游 `error.incomplete`，zh 逐字）。
pub const ERROR_INCOMPLETE_LABEL: &str = "请先完成这道问题。";

/// 跳过按钮（上游 `action.skip`，zh 逐字）。
pub const ACTION_SKIP_LABEL: &str = "跳过";

/// 自定义答案占位（上游 `custom.placeholder`，zh 逐字）。
pub const CUSTOM_PLACEHOLDER_LABEL: &str = "输入你的答案";

/// 放弃整组问题（上游 `nav.cancel`，zh 逐字）。
pub const NAV_CANCEL_LABEL: &str = "放弃整组问题";

/// 推荐徽标（上游 `option.recommended`，zh 逐字）。
pub const OPTION_RECOMMENDED_LABEL: &str = "推荐";

/// 下一题按钮（上游 `action.next`，zh 逐字）。
pub const ACTION_NEXT_LABEL: &str = "下一题";

/// 推荐标记剥离（上游 `parseRecommendedLabel`：`\s*(?:\((?:recommended|推
/// 荐)\)|（(?:recommended|推荐)）)\s*$/i`——命中返回剥标后的正文）。
pub fn parse_recommended_label(label: &str) -> Option<String> {
    let trimmed = label.trim_end();
    let lower = trimmed.to_lowercase();
    for suffix in ["(recommended)", "（recommended）", "(推荐)", "（推荐）"] {
        if lower.ends_with(suffix) {
            let head = &trimmed[..trimmed.len() - suffix.len()];
            return Some(head.trim().to_string());
        }
    }
    None
}

/// 一次待答提问卡的完整状态（分页单题；跨页缓存每题选择与自定义答案）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuestionCardState {
    pub call_id: String,
    pub questions: Vec<AskUserQuestion>,
    /// 当前页（0 基）。
    pub index: usize,
    /// 每题选中的选项下标（单选至多一项；多选任意）。
    pub selected: Vec<Vec<usize>>,
    /// 每题自定义答案（与输入框翻页时互同步）。
    pub customs: Vec<String>,
    /// 每题显式跳过（跳过清空该题选择与自定义）。
    pub skipped: Vec<bool>,
    /// 提交校验错误（error.incomplete；提交成功即关卡）。
    pub error: Option<String>,
}

impl QuestionCardState {
    /// 开卡：推荐项隐式预选（上游 `recommendedOptions`）。
    pub fn new(call_id: impl Into<String>, questions: Vec<AskUserQuestion>) -> Self {
        let selected: Vec<Vec<usize>> = questions
            .iter()
            .map(|q| {
                q.options
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                    .filter_map(|(i, o)| parse_recommended_label(&o.label).map(|_| i))
                    .collect()
            })
            .collect();
        let n = questions.len();
        Self {
            call_id: call_id.into(),
            questions,
            index: 0,
            selected,
            customs: vec![String::new(); n],
            skipped: vec![false; n],
            error: None,
        }
    }

    pub fn total(&self) -> usize {
        self.questions.len()
    }

    /// 当前题是否被推荐（供渲染徽标：剥标正文 + badge）。
    pub fn current_options(&self) -> Vec<(String, Option<String>, bool)> {
        let Some(q) = self.questions.get(self.index) else { return Vec::new() };
        let skipped = self.skipped[self.index];
        q.options
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|o| {
                let label = if skipped { o.label.clone() } else { parse_recommended_label(&o.label).unwrap_or_else(|| o.label.clone()) };
                (label, o.description.clone(), !skipped && parse_recommended_label(&o.label).is_some())
            })
            .collect()
    }

    /// 选项点击（单选清他项；多选切换；跳过解除；错误态清除）。
    pub fn toggle_option(&mut self, option_index: usize) {
        let qi = self.index;
        if qi >= self.total() || self.skipped[qi] {
            return;
        }
        let Some(opts) = self.questions[qi].options.as_ref() else { return };
        if option_index >= opts.len() {
            return;
        }
        let multi = self.questions[qi].multi_select.unwrap_or(false);
        let picks = &mut self.selected[qi];
        if multi {
            match picks.iter().position(|&i| i == option_index) {
                Some(pos) => {
                    picks.remove(pos);
                }
                None => picks.push(option_index),
            }
        } else if picks.len() == 1 && picks[0] == option_index {
            picks.clear();
        } else {
            *picks = vec![option_index];
        }
        self.error = None;
    }

    /// 跳过切换（跳过清空该题选择；错误态清除）。
    pub fn toggle_skip(&mut self) {
        let qi = self.index;
        if qi >= self.total() {
            return;
        }
        self.skipped[qi] = !self.skipped[qi];
        if self.skipped[qi] {
            self.selected[qi].clear();
            self.customs[qi].clear();
        }
        self.error = None;
    }

    /// 当前题是否被跳过。
    pub fn current_skipped(&self) -> bool {
        self.skipped.get(self.index).copied().unwrap_or(false)
    }

    /// 题面是否未完成（未跳过 && 无选中 && 自定义为空白——无选项的开放题
    /// 必须跳过或作答）。
    fn incomplete(&self, qi: usize) -> bool {
        if self.skipped[qi] {
            return false;
        }
        self.selected[qi].is_empty() && self.customs[qi].trim().is_empty()
    }

    /// 当前题是否有选中项。
    pub fn current_selected(&self) -> &[usize] {
        self.selected.get(self.index).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 翻页缓存当前输入框值（宿主在翻页/提交前调用）。
    pub fn cache_custom(&mut self, text: impl Into<String>) {
        let qi = self.index;
        if qi < self.total() {
            self.customs[qi] = text.into();
        }
    }

    /// 当前题的自定义答案（翻回时宿主灌输入框）。
    pub fn current_custom(&self) -> &str {
        self.customs.get(self.index).map(|s| s.as_str()).unwrap_or("")
    }

    /// 下一题（钳尾）。
    pub fn page_next(&mut self) -> bool {
        if self.index + 1 < self.total() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// 上一题（钳头）。
    pub fn page_prev(&mut self) -> bool {
        if self.index > 0 {
            self.index -= 1;
            true
        } else {
            false
        }
    }

    /// 是否最后一页（尾页按钮 = 提交）。
    pub fn on_last_page(&self) -> bool {
        self.index + 1 >= self.total()
    }

    /// 校验全部题已答或已跳过；未完成题号（0 基）列表，空 = 可提交。
    pub fn incomplete_questions(&self) -> Vec<usize> {
        (0..self.total()).filter(|&qi| self.incomplete(qi)).collect()
    }

    /// 提交答案批（有未完成题 = None；跳过题 = 空选无自定义——上游 skip
    /// 编码同形；自定义空白视为无自定义）。
    pub fn build_answers(&self) -> Option<Vec<AskUserAnswerItem>> {
        if !self.incomplete_questions().is_empty() {
            return None;
        }
        Some(
            self.questions
                .iter()
                .enumerate()
                .map(|(qi, q)| {
                    if self.skipped[qi] {
                        return AskUserAnswerItem {
                            id: q.id.clone(),
                            selected: Vec::new(),
                            custom: None,
                        };
                    }
                    let labels: Vec<String> = self.selected[qi]
                        .iter()
                        .filter_map(|&oi| {
                            q.options.as_ref().and_then(|o| o.get(oi)).map(|opt| opt.label.clone())
                        })
                        .collect();
                    let custom = {
                        let raw = self.customs[qi].trim();
                        if raw.is_empty() { None } else { Some(raw.to_string()) }
                    };
                    AskUserAnswerItem { id: q.id.clone(), selected: labels, custom }
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_tools::AskUserOption;

    fn question(id: &str, labels: &[&str], multi: Option<bool>) -> AskUserQuestion {
        AskUserQuestion {
            id: id.into(),
            question: format!("题 {id}"),
            header: Some("选择".into()),
            options: Some(
                labels
                    .iter()
                    .map(|l| AskUserOption { label: (*l).into(), description: None })
                    .collect(),
            ),
            multi_select: multi,
        }
    }

    #[test]
    fn recommended_labels_parse_and_preselect() {
        assert_eq!(
            parse_recommended_label("方案 A (Recommended)"),
            Some("方案 A".into())
        );
        assert_eq!(parse_recommended_label("方案 B（推荐）"), Some("方案 B".into()));
        assert_eq!(parse_recommended_label("方案 B (推荐)"), Some("方案 B".into()));
        assert_eq!(parse_recommended_label("普通选项"), None);
        let mut card = QuestionCardState::new(
            "call1",
            vec![question("q1", &["方案 A (Recommended)", "方案 B"], None)],
        );
        assert_eq!(card.current_selected(), &[0]);
        // 推荐项渲染剥标 + 徽标
        assert_eq!(
            card.current_options(),
            vec![("方案 A".to_string(), None, true), ("方案 B".to_string(), None, false)]
        );
        // 点击他项覆盖预选
        card.toggle_option(1);
        assert_eq!(card.current_selected(), &[1]);
        assert_eq!(card.build_answers().unwrap()[0].selected, vec!["方案 B"]);
    }

    #[test]
    fn single_select_replaces_multi_select_accumulates() {
        let mut multi = QuestionCardState::new("c", vec![question("q1", &["a", "b", "c"], Some(true))]);
        multi.toggle_option(0);
        multi.toggle_option(2);
        assert_eq!(multi.current_selected(), &[0, 2]);
        multi.toggle_option(0);
        assert_eq!(multi.current_selected(), &[2]);
        let mut single = QuestionCardState::new("c", vec![question("q2", &["a", "b"], None)]);
        single.toggle_option(0);
        single.toggle_option(1);
        assert_eq!(single.current_selected(), &[1]);
    }

    #[test]
    fn skip_clears_selection_and_encodes_empty_batch() {
        let mut card = QuestionCardState::new("c", vec![question("q1", &["a", "b"], None)]);
        card.toggle_option(0);
        card.toggle_skip();
        assert_eq!(card.current_selected(), &[] as &[usize]);
        assert!(card.current_skipped());
        let answers = card.build_answers().unwrap();
        assert_eq!(answers[0].selected, Vec::<String>::new());
        assert_eq!(answers[0].custom, None);
    }

    #[test]
    fn incomplete_submission_is_rejected_with_pagination() {
        let mut card = QuestionCardState::new(
            "c",
            vec![question("q1", &["a", "b"], None), question("q2", &["x", "y"], None)],
        );
        // 无推荐 → 两题都未选 = 未完成
        assert_eq!(card.incomplete_questions(), vec![0, 1]);
        assert_eq!(card.build_answers(), None);
        card.toggle_option(0);
        assert!(card.page_next());
        assert!(!card.page_next(), "尾页再翻无效");
        assert!(card.on_last_page());
        // 第二页跳过 → q2 可过，但 q1 选中生效 → 整批可提交
        card.toggle_skip();
        let answers = card.build_answers().unwrap();
        assert_eq!(answers[0].selected, vec!["a"]);
        assert_eq!(answers[1].selected, Vec::<String>::new(), "跳过题编码为空选");
    }

    #[test]
    fn custom_answer_trims_and_rides_the_batch() {
        let mut card = QuestionCardState::new("c", vec![question("q1", &["a"], None)]);
        card.cache_custom("  自己写  ");
        let answers = card.build_answers().unwrap();
        assert_eq!(answers[0].custom.as_deref(), Some("自己写"));
        assert!(answers[0].selected.is_empty());
        // 无选项的开放题：不作答 = 未完成（必须跳过或作答）
        let mut no_opts = QuestionCardState::new(
            "c",
            vec![AskUserQuestion {
                id: "q9".into(),
                question: "开放题".into(),
                header: None,
                options: None,
                multi_select: None,
            }],
        );
        assert_eq!(no_opts.incomplete_questions(), vec![0]);
        no_opts.cache_custom("答案");
        assert!(no_opts.incomplete_questions().is_empty());
        // 跳过同样放行
        let mut skipped = QuestionCardState::new(
            "c",
            vec![AskUserQuestion {
                id: "q9".into(),
                question: "开放题".into(),
                header: None,
                options: None,
                multi_select: None,
            }],
        );
        skipped.toggle_skip();
        assert!(skipped.incomplete_questions().is_empty());
    }
}
