//! DeepSeek Harness（`dsh`）引擎适配。
//!
//! 与另外几家不同，dsh 有两套数据面：
//!
//! - **实时**：`dsh --profile acp` 起一个 stdio JSON-RPC 进程，用标准 ACP v1
//!   （`session/new` / `session/prompt` / `session/update` / `session/cancel`）
//!   驱动会话。这是官方给"外部控制器"准备的入口，不碰它的内部结构。
//! - **历史**：`$DSH_HOME/sessions/<编码 cwd>/<会话 id>/session.v4.jsonl.zstd`，
//!   append-only 的 zstd 压缩 JSONL。ACP 明确不支持 `session/load`（不重放历史），
//!   所以回溯只能读这份日志。
//!
//! 分层：
//! - `locator`    `$DSH_HOME` / sessions 目录 / CLI 可达性
//! - `acp`        ACP v1 的 JSON-RPC 客户端（进程 + 帧收发 + 服务端请求应答）
//! - `runtime`    把 ACP 的语义更新翻译成 Prism 的 engine-event
//! - `history`    zstd JSONL → 会话列表 / 时间线
//! - `permission` 权限档位（只能靠进程环境传，见模块注释）
//! - `engine`     `Engine` trait 实现

pub mod acp;
pub mod engine;
pub mod history;
pub mod locator;
pub mod permission;
pub mod runtime;

/// 会话 id 与项目 id 共用前缀：ACP 返回的是裸 UUID，不加前缀就会和 codex 的
/// UUID 撞在一起；项目 id 是 cwd，靠调用点区分（与 codex 的做法一致）。
pub const ID_PREFIX: &str = "dsh:";

/// dsh 会话 id：`dsh:<uuid>`。
pub fn session_id_of(raw: &str) -> String {
    format!("{ID_PREFIX}{raw}")
}

/// 剥掉前缀拿回 ACP 认的那个 id。
pub fn raw_session_id(session_id: &str) -> Option<&str> {
    session_id
        .strip_prefix(ID_PREFIX)
        .filter(|raw| !raw.trim().is_empty())
}

pub fn is_dsh_session(session_id: &str) -> bool {
    raw_session_id(session_id).is_some()
}

/// 项目 id：cwd 编码进去（与 codex 同形）。
pub fn project_id(directory: &str) -> String {
    format!("{ID_PREFIX}{directory}")
}

pub fn is_dsh_project(project_id_value: &str) -> bool {
    project_id_value.starts_with(ID_PREFIX)
}

/// 从项目 id 还原工作目录。
pub fn directory_of(project_id_value: &str) -> Option<&str> {
    project_id_value.strip_prefix(ID_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_round_trip() {
        let raw = "11514f32-760e-4f23-b730-61669703fb02";
        let wrapped = session_id_of(raw);
        assert_eq!(wrapped, "dsh:11514f32-760e-4f23-b730-61669703fb02");
        assert!(is_dsh_session(&wrapped));
        assert_eq!(raw_session_id(&wrapped), Some(raw));
    }

    #[test]
    fn bare_uuid_is_not_a_dsh_session() {
        // 不加前缀的 UUID 可能是 codex 的，不能认。
        assert!(!is_dsh_session("11514f32-760e-4f23-b730-61669703fb02"));
        assert!(!is_dsh_session("dsh:"));
    }

    #[test]
    fn project_ids_carry_the_working_directory() {
        let id = project_id("/tmp/demo");
        assert!(is_dsh_project(&id));
        assert_eq!(directory_of(&id), Some("/tmp/demo"));
    }
}
