//! 上下文明细：这一轮发出去的东西，各占多少。
//!
//! 引擎只报一个总数（`input`），说不出"系统提示 / 工具定义 / 对话"各多少。
//! 这里按**字符数**把它们量出来，前端再按比例把总数分摊下去——比例是估的，
//! 总数是真的，所以面板底部必须写清楚这一点。
//!
//! 只对原生引擎有意义：另外几家把系统提示和工具藏在各自的应用里，我们拿不到。

use serde::Serialize;

use super::{agent, store};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextStats {
    /// 系统提示词（含工作目录与规则）。
    pub system: usize,
    /// 工具声明（OpenAI function schema 的 JSON 长度）。
    pub tools: usize,
    /// 用户消息。
    pub user: usize,
    /// 助手消息（含它发出的工具调用入参）。
    pub assistant: usize,
    /// 工具结果（读到的文件内容、命令输出）。
    pub tool: usize,
}

/// 量一下这个会话发出去的每一类文本有多大（字符数）。
pub fn stats(session_id: &str) -> Option<ContextStats> {
    let raw = super::raw_session_id(session_id)?;
    let meta = store::load_meta(raw)?;

    let system = agent::system_prompt(&meta.directory).chars().count();
    // 工具声明按实际发出去的算：六个内置 + task 子代理。
    let tools = serde_json::to_string(&agent::tool_specs())
        .map(|text| text.chars().count())
        .unwrap_or(0);

    let mut user = 0usize;
    let mut assistant = 0usize;
    let mut tool = 0usize;

    for message in store::messages(raw) {
        let mut size = message.text.chars().count();
        // 工具调用入参也是助手"说"出去的话，算在它头上。
        for call in &message.tool_calls {
            size += call.name.chars().count() + call.arguments.chars().count();
        }
        match message.role.as_str() {
            // 压缩摘要以 user 消息发给模型，算进「用户」那一格才对得上号。
            "user" | "compact" => user += size,
            "assistant" => assistant += size,
            "tool" => tool += size,
            _ => {}
        }
    }

    Some(ContextStats {
        system,
        tools,
        user,
        assistant,
        tool,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_positive_for_a_fresh_session() {
        // 新建一个临时会话目录级别的会话，量出来至少系统提示与工具声明不为空。
        let dir = std::env::temp_dir().join(format!("prism-ctx-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let meta = store::create(dir.to_str().unwrap()).expect("建会话");
        let stats = stats(&format!("native:{}", meta.id)).expect("统计");
        assert!(stats.system > 20, "系统提示不该是空的");
        assert!(stats.tools > 200, "工具声明不该是空的");
        assert_eq!(stats.user, 0);
        assert_eq!(stats.assistant, 0);
        assert_eq!(stats.tool, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_session_returns_none() {
        assert!(stats("native:n-does-not-exist").is_none());
        assert!(stats("opencode:abc").is_none());
    }
}
