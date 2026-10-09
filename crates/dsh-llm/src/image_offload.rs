//! Image offload 纯函数层（上游 dsh-llm `content.ts` offload 三件套：
//! `offloadedImageText` / `projectOffloadedImages` / `requiredImageOffload`）。
//!
//! 预算超限时路由抛 `IMAGE_OFFLOAD_REQUIRED`（4b：适配器预算），恢复执行器
//! 把最旧出现的若干图片标记为 offloaded 并落 `image/offload` 耐久事件
//! （4c：恢复循环）——本模块只承载无状态的标记/投影/计数语义。

use crate::message::Message;
use crate::types::{ContentBlock, ImageAttachmentRef};

/// 请求图片的表示形态（上游 budget.representation：raw=Files/原字节，
/// base64=内联回退按编码长度计）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageRepresentation {
    Raw,
    Base64,
}

/// 单路由的请求图片预算（上游 `LlmImageRequestBudget` 子集）。
/// quantum 为移除步进：超出按整 quantum 圆整后再取最旧前缀。
#[derive(Clone, Copy, Debug)]
pub struct ImageRequestBudget {
    pub representation: ImageRepresentation,
    pub max_bytes: Option<u64>,
    pub max_images: Option<usize>,
    /// 移除的字节步进（None = 1，逐字节）。
    pub byte_quantum: Option<u64>,
    /// 移除的个数步进（None = 1，逐张）。
    pub count_quantum: Option<usize>,
}

/// 归一化副本的执行环境可读路径（上游 ImageAttachmentAccess）。
#[derive(Clone, Debug)]
pub struct ImageAccess {
    pub readonly_path: String,
}

/// base64 编码长度（含 padding）：ceil(bytes/3)*4。
pub fn base64_length(bytes: u64) -> u64 {
    bytes.div_ceil(3) * 4
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}

/// 单张附件的稳定标识（上游 imageIdentity：有名字给 `"name" (id)`，否则裸 id）。
fn image_identity(reference: &ImageAttachmentRef) -> String {
    match &reference.name {
        Some(name) => format!("{} ({})", quoted(name), reference.attachment_id),
        None => reference.attachment_id.clone(),
    }
}

fn extension(media_type: &str) -> String {
    match media_type {
        "image/jpeg" => ".jpg".into(),
        other => match other.strip_prefix("image/") {
            Some(suffix) => format!(".{suffix}"),
            None => ".png".into(),
        },
    }
}

fn normalized_access_text(reference: &ImageAttachmentRef, access: &ImageAccess) -> String {
    format!(
        " Normalized copy (read-only; may be resized or re-encoded): {} ({}x{}px, {}).\u{20}\
         Source dimensions, format, and byte size may differ.\u{20}\
         Copy to a writable path ending in {} before editing.",
        quoted(&access.readonly_path),
        reference.width,
        reference.height,
        reference.media_type,
        extension(&reference.media_type),
    )
}

/// 单次请求限额遗漏的稳定占位文本（上游 offloadedImageText）：携带附件
/// 标识与可用恢复路径；无路径时指示向用户重取。
pub fn offloaded_image_text(reference: &ImageAttachmentRef, access: Option<&ImageAccess>) -> String {
    let identity =
        format!("image omitted to fit request image limits; {}.", image_identity(reference));
    match access {
        None => format!(
            "[{identity} No local normalized image path is available; \
             ask the user to attach it again if needed.]"
        ),
        Some(access) => format!("[{identity}{}]", normalized_access_text(reference, access)),
    }
}

/// 内容里是否存在图片块（上游 contentHasImage——所有图片策略共用的唯一遍历）。
pub fn content_has_image(content: &[ContentBlock]) -> bool {
    content
        .iter()
        .any(|b| matches!(b, ContentBlock::Image { .. }))
}

/// 把每条消息中 offloaded 的图片出现替换为占位文本（上游
/// projectOffloadedImages）：offloaded 集是耐久面事实，所有路由发送同一
/// 集合，仅占位文案归路由所有；无 offloaded 时原样返回等价列表。
pub fn project_offloaded_images(
    messages: &[Message],
    placeholder: impl Fn(&ImageAttachmentRef) -> String,
) -> Vec<Message> {
    messages
        .iter()
        .map(|message| {
            let mut changed = false;
            let content: Vec<ContentBlock> = message
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Image { attachment, offloaded: true } => {
                        changed = true;
                        ContentBlock::text(placeholder(attachment))
                    }
                    other => other.clone(),
                })
                .collect();
            if changed {
                let mut next = message.clone();
                next.content = content;
                next
            } else {
                message.clone()
            }
        })
        .collect()
}

/// 超预算时需移除的最旧出现数（上游 offloadedImagePrefixCount）：整 quantum
/// 圆整后取最旧前缀；byte 目标在 quantum=1 时用 >=、>1 时用 >（严格超出语义
/// ——移除量必须真正跨过圆整目标）。
fn offloaded_image_prefix_count(lengths: &[u64], budget: &ImageRequestBudget) -> usize {
    let total: u64 = lengths.iter().sum();
    let excess_count = match budget.max_images {
        Some(max) => lengths.len().saturating_sub(max),
        None => 0,
    };
    let excess_bytes = match budget.max_bytes {
        Some(max) => total.saturating_sub(max),
        None => 0,
    };
    if excess_count == 0 && excess_bytes == 0 {
        return 0;
    }
    let count_quantum = budget.count_quantum.unwrap_or(1).max(1);
    let byte_quantum = budget.byte_quantum.unwrap_or(1).max(1);
    let remove_count = if excess_count == 0 {
        0
    } else {
        excess_count.div_ceil(count_quantum) * count_quantum
    };
    let remove_bytes = if excess_bytes == 0 {
        0
    } else {
        excess_bytes.div_ceil(byte_quantum) * byte_quantum
    };
    let mut count = 0usize;
    let mut removed_bytes = 0u64;
    for image_bytes in lengths {
        let byte_target_met = remove_bytes == 0
            || if byte_quantum == 1 {
                removed_bytes >= remove_bytes
            } else {
                removed_bytes > remove_bytes
            };
        if count >= remove_count && byte_target_met {
            break;
        }
        removed_bytes += image_bytes;
        count += 1;
    }
    count
}

/// 派生请求在给定预算下仍需 offload 的最旧保留出现数（上游
/// requiredImageOffload）：跳过已 offloaded 的出现，按路由表示计长
/// （base64 用编码长度），超限时不自行移除而以此计数抛
/// `IMAGE_OFFLOAD_REQUIRED`。
pub fn required_image_offload(
    messages: &[Message],
    budget: &ImageRequestBudget,
    version_bytes: impl Fn(&ImageAttachmentRef) -> u64,
) -> usize {
    let mut lengths: Vec<u64> = Vec::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::Image { attachment, offloaded: false } = block {
                let bytes = version_bytes(attachment);
                lengths.push(match budget.representation {
                    ImageRepresentation::Base64 => base64_length(bytes),
                    ImageRepresentation::Raw => bytes,
                });
            }
        }
    }
    offloaded_image_prefix_count(&lengths, budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::types::{CallId, ContentBlock};

    fn image_ref(name: Option<&str>, bytes: u64) -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: format!("sha256:{:064x}", bytes),
            name: name.map(str::to_string),
            media_type: "image/png".into(),
            bytes,
            width: 10,
            height: 10,
            original_dimensions: None,
        }
    }

    fn image_block(name: Option<&str>, bytes: u64, offloaded: bool) -> ContentBlock {
        ContentBlock::Image { attachment: image_ref(name, bytes), offloaded }
    }

    fn user_message(content: Vec<ContentBlock>) -> Message {
        Message::new(crate::message::Role::User, content, crate::message::MessageSource::User)
    }

    #[test]
    fn base64_length_matches_ceil_bytes_over_three_times_four() {
        assert_eq!(base64_length(0), 0);
        assert_eq!(base64_length(1), 4);
        assert_eq!(base64_length(3), 4);
        assert_eq!(base64_length(4), 8);
        assert_eq!(base64_length(30_000_000), 40_000_000);
    }

    #[test]
    fn offload_text_names_identity_without_path_and_with_path() {
        let r = image_ref(Some("cat.png"), 100);
        let bare = offloaded_image_text(&r, None);
        assert!(bare.starts_with(
            "[image omitted to fit request image limits; \"cat.png\" (sha256:"
        ), "{bare}");
        assert!(bare.contains("No local normalized image path is available"), "{bare}");
        let access = ImageAccess { readonly_path: "/tmp/normalized/cat.png".into() };
        let with = offloaded_image_text(&r, Some(&access));
        assert!(with.contains("\"/tmp/normalized/cat.png\" (10x10px, image/png)"), "{with}");
        assert!(with.contains("ending in .png before editing"), "{with}");
        // 无名字形态：裸 attachmentId
        let unnamed = image_ref(None, 7);
        assert!(offloaded_image_text(&unnamed, None).contains(&unnamed.attachment_id));
        // jpeg 扩展名映射
        let mut jpeg = image_ref(Some("a"), 1);
        jpeg.media_type = "image/jpeg".into();
        assert!(offloaded_image_text(&jpeg, Some(&access)).contains("ending in .jpg"));
    }

    #[test]
    fn project_replaces_only_offloaded_occurrences() {
        let messages = vec![
            user_message(vec![
                image_block(Some("a"), 1, true),
                ContentBlock::text("between"),
                image_block(Some("b"), 2, false),
            ]),
            user_message(vec![ContentBlock::ToolResult {
                tool_call_id: CallId("c1".into()),
                content: vec![image_block(Some("nested"), 3, true)],
                is_error: None,
            }]),
        ];
        let out = project_offloaded_images(&messages, |r| format!("P:{}", r.name.clone().unwrap()));
        // 第一条：offloaded → 文本；保留块原样
        assert!(matches!(&out[0].content[0], ContentBlock::Text { text } if text == "P:a"));
        assert!(matches!(&out[0].content[1], ContentBlock::Text { text } if text == "between"));
        assert!(matches!(&out[0].content[2], ContentBlock::Image { offloaded: false, .. }));
        // 嵌套 tool-result 内的 offloaded 不被顶替（上游投影只走消息顶层；
        // 工具结果内的图片由 image_url 序列化面处理）
        assert!(matches!(&out[1].content[0], ContentBlock::ToolResult { .. }));
        // 原输入未被修改（clone 语义）
        assert!(matches!(&messages[0].content[0], ContentBlock::Image { offloaded: true, .. }));
    }

    #[test]
    fn required_offload_counts_only_retained_leading_occurrences() {
        let budget = |max_images, max_bytes| ImageRequestBudget {
            representation: ImageRepresentation::Raw,
            max_bytes,
            max_images,
            byte_quantum: None,
            count_quantum: None,
        };
        let messages = vec![user_message(vec![
            image_block(Some("a"), 100, false),
            image_block(Some("b"), 200, false),
        ])];
        assert_eq!(required_image_offload(&messages, &budget(None, None), |r| r.bytes), 0);
        assert_eq!(required_image_offload(&messages, &budget(Some(2), None), |r| r.bytes), 0);
        assert_eq!(required_image_offload(&messages, &budget(Some(1), None), |r| r.bytes), 1);
        assert_eq!(required_image_offload(&messages, &budget(None, Some(250)), |r| r.bytes), 1);
        // 已 offloaded 的出现不参与计数
        let marked = vec![user_message(vec![
            image_block(Some("a"), 100, true),
            image_block(Some("b"), 200, false),
        ])];
        assert_eq!(required_image_offload(&marked, &budget(None, Some(250)), |r| r.bytes), 0);
    }

    #[test]
    fn quanta_round_up_and_strict_byte_semantics() {
        // count quantum：超 1 张、步进 2 → 移 2 张
        let budget = ImageRequestBudget {
            representation: ImageRepresentation::Raw,
            max_bytes: None,
            max_images: Some(1),
            byte_quantum: None,
            count_quantum: Some(2),
        };
        let messages = vec![user_message(vec![
            image_block(None, 1, false),
            image_block(None, 1, false),
            image_block(None, 1, false),
        ])];
        assert_eq!(required_image_offload(&messages, &budget, |r| r.bytes), 2);
        // byte quantum > 1：removed_bytes 必须 > 圆整目标（严格超出）
        let b = ImageRequestBudget {
            representation: ImageRepresentation::Raw,
            max_bytes: Some(100),
            max_images: None,
            byte_quantum: Some(100),
            count_quantum: None,
        };
        // 超出 1 字节 → 圆整目标 100；首张 50 字节后 50（不>100）继续；两张后
        // 100（仍不>100）→ 第三张触发
        let msgs = vec![user_message(vec![
            image_block(None, 50, false),
            image_block(None, 50, false),
            image_block(None, 1, false),
        ])];
        assert_eq!(required_image_offload(&msgs, &b, |r| r.bytes), 3);
        // quantum=1：>= 语义，达到即停
        let b1 = ImageRequestBudget {
            representation: ImageRepresentation::Base64,
            max_bytes: Some(8),
            max_images: None,
            byte_quantum: None,
            count_quantum: None,
        };
        // 10 字节 → base64 16，超出 8；首张 base64 8 → removed 8 >= 8 停 → 1 张
        let msgs1 = vec![user_message(vec![
            image_block(None, 6, false),
            image_block(None, 4, false),
        ])];
        assert_eq!(required_image_offload(&msgs1, &b1, |r| r.bytes), 1);
    }
}
