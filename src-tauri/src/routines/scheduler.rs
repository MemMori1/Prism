//! 定时任务调度：应用内 tick 循环（承载方式取舍见 ROADMAP M11）。
//!
//! 决策：首版用应用内循环（30s tick），不依赖 launchd——触发只在本进程内，
//! 无需安装 plist 与独立 runner 二进制；代价是应用未运行时任务不跑。
//! 错过语义：应用关停期间的错过不补跑（启动即重置窗口）；系统休眠唤醒后
//! 按 tick 窗口合并补一次（与 launchd 的 StartCalendarInterval 行为近似）。

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

use super::{cron, runner, store};

/// 运行中的任务集合（调度器与"立即运行"共用）。
pub struct RoutinesState {
    running: Mutex<HashSet<String>>,
}

impl RoutinesState {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(HashSet::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.running.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.lock().contains(id)
    }

    pub fn try_mark_running(&self, id: &str) -> bool {
        let mut set = self.lock();
        if set.contains(id) {
            return false;
        }
        set.insert(id.to_string());
        true
    }

    pub fn unmark_running(&self, id: &str) {
        self.lock().remove(id);
    }
}

impl Default for RoutinesState {
    fn default() -> Self {
        Self::new()
    }
}

const TICK: Duration = Duration::from_secs(30);

/// 启动调度线程（进程内唯一）。
pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_check = runner::now_ms();
        loop {
            std::thread::sleep(TICK);
            let now = runner::now_ms();
            check_and_fire(&app, last_check, now);
            last_check = now;
        }
    });
}

fn check_and_fire(app: &AppHandle, from_ms: i64, to_ms: i64) {
    // 调度线程比插件活得久：`routines` 被卸掉后线程还在转，这里必须能
    // "什么都不做"地返回，而不是 `state()` panic。没插件就没主意要执行。
    let Some(state) = app.try_state::<RoutinesState>() else {
        return;
    };
    let Some(path) = store::store_path() else {
        return;
    };
    let routines = match store::load_all(&path) {
        Ok(routines) => routines,
        Err(error) => {
            eprintln!("[prism] 读取例行任务失败：{error}");
            return;
        }
    };
    for routine in routines.iter().filter(|item| item.enabled) {
        let Ok(schedule) = cron::parse(&routine.schedule) else {
            continue;
        };
        let due = schedule
            .next_after(from_ms)
            .map(|next| next <= to_ms)
            .unwrap_or(false);
        if !due {
            continue;
        }
        if state.is_running(&routine.id) {
            continue;
        }
        if let Err(error) = runner::run_in_background(app, routine.clone()) {
            eprintln!("[prism] 触发例行任务失败（{}）：{error}", routine.name);
        }
    }
}
