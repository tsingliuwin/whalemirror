//! 已回答提问的转录面（上游 `ui-user-questions` answeredQuestionDefinition /
//! QuestionReplyView + `ui-tool` AskQuestionRow 的 rustdsh 等价判定面）。
//!
//! 提问工具行（ask_user_question）状态摘要与已回答批次气泡的行集都在这
//! 里解析（入参 questions + 结果答案批 JSON）；渲染在宿主 chat 转录。

/// 工具行标题（上游 `ask.rowTitle`，zh 逐字）。
pub const ASK_ROW_TITLE: &str = "提问";
/// 运行中摘要（上游 `ask.waiting`，zh 逐字）。
pub const ASK_WAITING_LABEL: &str = "等待回答";
/// 已中断摘要（上游 `ask.interrupted`，zh 逐字）。
pub const ASK_INTERRUPTED_LABEL: &str = "已中断";
/// 已取消摘要（上游 `ask.cancelled`，zh 逐字）。
pub const ASK_CANCELLED_LABEL: &str = "已取消";
/// 工具卡内未回答行（上游 `ask.skipped`，zh 逐字）。
pub const ASK_SKIPPED_LABEL: &str = "未回答";
/// 气泡内跳过行（上游 `reply.skipped`，zh 逐字）。
pub const REPLY_SKIPPED_LABEL: &str = "已跳过";

/// 上游 `ask.answered`：`{answered}/{total} 已回答`（zh 模板逐字）。
pub fn answered_summary(answered: usize, total: usize) -> String {
    format!("{answered}/{total} 已回答")
}

/// 一题的气泡行（题面 + 答案值集；空集 = 跳过）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionReplyLine {
    pub header: Option<String>,
    pub question: String,
    pub answers: Vec<String>,
    pub skipped: bool,
}

/// 解析模型入参 questions（(id, header, question) 三元；形不合法 = None）。
pub fn parse_questions(arguments: &str) -> Option<Vec<(String, Option<String>, String)>> {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments).ok()?;
    let items = parsed.get("questions")?.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let id = item.get("id")?.as_str()?.to_string();
                let question = item.get("question")?.as_str()?.to_string();
                let header = item.get("header").and_then(|v| v.as_str()).map(str::to_string);
                Some((id, header, question))
            })
            .collect(),
    )
}

/// 解析答案批结果 JSON（`{"answers":[{id,selected,custom?}]}`；非该形 = None
/// ——运行中（无结果）/错误文本/其他工具结果自然落 None）。
pub fn parse_answers(result: &str) -> Option<Vec<(String, Vec<String>, Option<String>)>> {
    let parsed = serde_json::from_str::<serde_json::Value>(result).ok()?;
    let items = parsed.get("answers")?.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let id = item.get("id")?.as_str()?.to_string();
                let selected: Vec<String> = item
                    .get("selected")?
                    .as_array()?
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                let custom = item.get("custom").and_then(|v| v.as_str()).map(str::to_string);
                Some((id, selected, custom))
            })
            .collect(),
    )
}

/// 一题是否算「已回答」（有选中项或有非空白自定义——上游 summary 的计数）。
fn answered_item(selected: &[String], custom: Option<&str>) -> bool {
    !selected.is_empty() || custom.is_some_and(|c| !c.trim().is_empty())
}

/// 工具行成功态摘要（`{answered}/{total} 已回答`）；入参或结果形不合法
/// = None（宿主走原 failure/通用摘要逻辑）。
pub fn row_summary(arguments: &str, result: &str) -> Option<String> {
    let questions = parse_questions(arguments)?;
    let answers = parse_answers(result)?;
    let answered = answers
        .iter()
        .filter(|(_, selected, custom)| answered_item(selected, custom.as_deref()))
        .count();
    Some(answered_summary(answered, questions.len()))
}

/// 气泡行集：按入参题序配答案（缺答案项 = 跳过）；答案值 = 选中标签后
/// 接非空白自定义（上游 dd 的 join ", " 语义）。
pub fn reply_lines(arguments: &str, result: &str) -> Option<Vec<QuestionReplyLine>> {
    let questions = parse_questions(arguments)?;
    let answers = parse_answers(result)?;
    Some(
        questions
            .into_iter()
            .map(|(id, header, question)| {
                let item = answers.iter().find(|(aid, _, _)| *aid == id);
                let (mut values, custom) = match item {
                    Some((_, selected, custom)) => (selected.clone(), custom.clone()),
                    None => (Vec::new(), None),
                };
                if let Some(c) = custom.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) {
                    values.push(c);
                }
                QuestionReplyLine {
                    header,
                    question,
                    skipped: values.is_empty(),
                    answers: values,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARGS: &str = r#"{"questions":[
        {"id":"q1","question":"用哪个方案？","header":"选择","options":[{"label":"方案 A"},{"label":"方案 B"}]},
        {"id":"q2","question":"补充说明？"},
        {"id":"q3","question":"要哪些特性？","multi_select":true}
    ]}"#;

    const ANSWERS: &str = r#"{"answers":[
        {"id":"q1","selected":["方案 B"]},
        {"id":"q2","selected":[],"custom":"  没有了  "},
        {"id":"q3","selected":[]}
    ]}"#;

    #[test]
    fn row_summary_counts_answered_items() {
        assert_eq!(row_summary(ARGS, ANSWERS).as_deref(), Some("2/3 已回答"));
    }

    #[test]
    fn row_summary_rejects_non_answer_shapes() {
        assert_eq!(row_summary(ARGS, "not json"), None);
        assert_eq!(row_summary(ARGS, r#"{"pending":true}"#), None);
        assert_eq!(row_summary("{}", ANSWERS), None);
    }

    #[test]
    fn reply_lines_pair_questions_with_answers_in_order() {
        let lines = reply_lines(ARGS, ANSWERS).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].header.as_deref(), Some("选择"));
        assert_eq!(lines[0].question, "用哪个方案？");
        assert_eq!(lines[0].answers, vec!["方案 B"]);
        assert!(!lines[0].skipped);
        // 自定义剥空白后追加
        assert_eq!(lines[1].answers, vec!["没有了"]);
        assert!(!lines[1].skipped);
        // 空选无自定义 = 跳过
        assert!(lines[2].skipped);
        assert!(lines[2].answers.is_empty());
    }

    #[test]
    fn bubble_skips_missing_answer_items() {
        let partial = r#"{"answers":[{"id":"q1","selected":["方案 B"]}]}"#;
        let lines = reply_lines(ARGS, partial).unwrap();
        assert!(!lines[0].skipped);
        assert!(lines[1].skipped, "缺答案项按跳过呈现");
        assert!(lines[2].skipped);
    }
}
