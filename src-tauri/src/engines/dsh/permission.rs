//! dsh 的权限档位。
//!
//! dsh 把权限拆成两个旋钮——沙箱范围（sandbox）与审批策略（approval）——再打包成
//! 「预设」。acp profile 的默认是 `workspace-write`：只能改工作区，且**每次都要问**。
//! 于是每个工具调用都会走 ACP 的 `session/request_permission` 反向请求，用户得在
//! Prism 里一张张点卡片。
//!
//! ACP 没有"读/改权限"的方法：`session/new` 返回的 `configOptions` 只有 `model` 与
//! `reasoning_effort`。权限**只从进程环境进**——acp profile 有三处配置读同一个变量
//! `DSH_PERMISSION_MODE`：
//!
//! ```yaml
//! - id: sandbox-policy
//!   config: { mode: !!js process.env.DSH_PERMISSION_MODE ?? 'workspace-write' }
//! - id: approval
//!   config: { policy: !!js (process.env.DSH_PERMISSION_MODE ?? 'workspace-write')
//!                     === 'danger-full-access' ? 'never' : 'ask' }
//! ```
//!
//! 也就是说 `danger-full-access` 同时意味着"全盘可写"和"从不询问"。档位挂在进程上，
//! 改完必须重启 dsh 才能生效——`ipc::dsh_set_permission` 保存后会调 `shutdown()`，
//! 下一次调用再懒启动时读到的就是新值。
//!
//! 存 `~/.prism/dsh/permission.json`：`{"mode":"danger-full-access"}`。
//! 文件不存在 = 不设这个变量，完全交给 dsh 自己的默认。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// dsh 读取的环变量名。
pub const ENV: &str = "DSH_PERMISSION_MODE";

/// 权限档位。三个值都是 dsh 认的预设名（与 profile 里的 `presets` 表一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 只能看：写文件与跑命令一律被沙箱拒绝。
    ReadOnly,
    /// 只能改工作目录，且每次工具调用都要审批（dsh 默认）。
    WorkspaceWrite,
    /// 全盘可写、从不询问。
    FullAccess,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ReadOnly => "read-only",
            Mode::WorkspaceWrite => "workspace-write",
            Mode::FullAccess => "danger-full-access",
        }
    }

    pub fn parse(value: &str) -> Option<Mode> {
        match value.trim() {
            "read-only" => Some(Mode::ReadOnly),
            "workspace-write" => Some(Mode::WorkspaceWrite),
            "danger-full-access" => Some(Mode::FullAccess),
            _ => None,
        }
    }

    /// 设置页用的一句话说明。
    pub fn description(self) -> &'static str {
        match self {
            Mode::ReadOnly => "只能读文件与列目录，写操作与命令直接拒绝",
            Mode::WorkspaceWrite => "只能改工作目录，且每次写文件/跑命令都要审批",
            Mode::FullAccess => "全盘可写，工具调用不再询问（谨慎开启）",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    mode: String,
}

fn path() -> Option<PathBuf> {
    let dir = crate::engines::opencode::search::prism_dir()?.join("dsh");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("permission.json"))
}

/// 当前档位。`None` = 没设置过，交给 dsh 默认。
///
/// 文件坏了也返回 `None` 而不是报错：这个文件只影响"要不要问"，读不出来时退回
/// dsh 自己的默认（更谨慎的那一档）永远比让会话起不来强。
pub fn load() -> Option<Mode> {
    let text = std::fs::read_to_string(path()?).ok()?;
    let stored: Stored = serde_json::from_str(&text).ok()?;
    Mode::parse(&stored.mode)
}

/// 保存档位。`None` 删掉文件（回到 dsh 默认）。
pub fn save(mode: Option<Mode>) -> Result<(), String> {
    let path = path().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    let Some(mode) = mode else {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            // 本来就没有：不是错误。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("删除 {} 失败：{error}", path.display())),
        };
    };
    let body = serde_json::to_string_pretty(&Stored {
        mode: mode.as_str().to_string(),
    })
    .map_err(|cause| format!("序列化权限档位失败：{cause}"))?;
    std::fs::write(&path, body).map_err(|error| format!("写入 {} 失败：{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_mode() {
        for mode in [Mode::ReadOnly, Mode::WorkspaceWrite, Mode::FullAccess] {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn rejects_unknown_values() {
        // 注意别把 dsh 的 `auto` 放进来：那是它自己注册的实验档位，不能由
        // 环境变量指定（`auto` 有准入检查），写进配置只会静默失效。
        assert_eq!(Mode::parse("auto"), None);
        assert_eq!(Mode::parse("full-access"), None);
        assert_eq!(Mode::parse(""), None);
    }

    #[test]
    fn env_var_name_matches_dsh_profile() {
        // 与 acp profile 里三处 `process.env.DSH_PERMISSION_MODE` 对齐。
        assert_eq!(ENV, "DSH_PERMISSION_MODE");
    }
}
