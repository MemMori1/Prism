//! `bin.jadx.decompile`：把 APK / DEX 反编译成 Java 源码树。
//!
//! 为什么单接一个反编译器：DEX 指令流能说明「调用了哪个方法」，但读不出
//! 「这段代码想干什么」。R8 混淆之后类名是 `LA;` / `LW/a;`，指令级能看懂
//! 控制流，却没法给行为命名——报告里那些「断点 B3–B8」多半卡在这一步。
//!
//! 只读样本：产物全部写在 `<工作目录>/_out/jadx/`，样本一个字节都不动。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::exec::{self, ExecSpec};
use super::{job_dir, require, stage_sample, StagedSample};
use crate::engines::native::tools::{
    builtin_full, Need, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome,
    ToolResult, ToolScope,
};

/// 20 MB 的包能跑几分钟；600s 给的是「大包 + 首次 JVM 冷启动」的余量。
const DECOMPILE_TIMEOUT: Duration = Duration::from_secs(600);
/// 制品输入复制到作业目录的上限。
const MAX_STAGED: u64 = 512 * 1024 * 1024;
/// 回给模型的路径预览条数：完整清单在磁盘上，让它按需 read_file。
const PREVIEW: usize = 40;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![builtin_full(
        "bin.jadx.decompile",
        "jadx 反编译成 Java（读代码语义）",
        schema,
        Permission::ReadOnly,
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["bin:static", "bin:decompile"],
            risk: Risk::Read,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Any,
            needs: &[Need::Backend("jadx")],
        },
        run,
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.jadx.decompile",
            "description": "用 jadx 把 APK / DEX 反编译成 Java 源码，落到工作区 `_out/jadx/`，返回类文件清单。**这是拿到「这段代码到底在干什么」的唯一入口**：DEX 指令级只能说明调了谁，读不出语义（R8 混淆后类名是 LA; 这种，控制流看懂了也没法命名）。只读样本、不执行样本。默认跳过资源（--no-res）。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "APK 或 DEX 路径（相对工作目录），或 DAG 制品引用 artifact:<id>"
                    },
                    "class": {
                        "type": "string",
                        "description": "只反编译一个类，**必须写完全限定类名**（带包名，如 app.orchard.tv.guard.HomeGuardService）——jadx 不认短名。不填就反编译全部（慢、产物多）。不确定全名先调 bin.dex.disasm（只填 path）列类名。"
                    },
                    "resources": {
                        "type": "boolean",
                        "description": "是否也反编译资源，默认 false（跳过更快，也少一堆 XML 噪音）"
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn run(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return ToolResult::fail("需要 path：APK 或 DEX 的路径".to_string());
    }
    let backend = match require("jadx") {
        Ok(status) => status,
        Err(error) => return ToolResult::fail(error),
    };
    let Some(program) = backend.path.clone() else {
        return ToolResult::fail("jadx 状态是 ready，但没有路径：点「重新检测」再试".to_string());
    };

    // 作业目录只放制品的临时副本（跑完即删）；源码树要留在工作区给模型读。
    let job = match job_dir("jadx") {
        Ok(dir) => dir,
        Err(error) => return ToolResult::fail(error),
    };
    let sample: StagedSample = match stage_sample(ctx, raw, &job, MAX_STAGED) {
        Ok(sample) => sample,
        Err(error) => return ToolResult::fail(error),
    };

    let out = Path::new(ctx.directory).join("_out").join("jadx");
    // 清掉上一次的产物：新旧混在一起会让模型读到过期的类。
    if out.exists() {
        if let Err(error) = std::fs::remove_dir_all(&out) {
            return ToolResult::fail(format!("清不掉上次的产物 {}：{error}", out.display()));
        }
    }
    if let Err(error) = std::fs::create_dir_all(&out) {
        return ToolResult::fail(format!("建不了输出目录 {}：{error}", out.display()));
    }

    let mut spec = ExecSpec::new(&program)
        .arg("-d")
        .arg(out.display().to_string())
        .timeout(DECOMPILE_TIMEOUT);
    if !args
        .get("resources")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        spec = spec.arg("--no-res");
    }
    if let Some(filter) = args.get("class").and_then(Value::as_str).map(str::trim) {
        if !filter.is_empty() {
            spec = spec.arg("--single-class").arg(filter);
        }
    }
    spec = spec.arg(sample.path.display().to_string());

    let output = match exec::run(&spec) {
        Ok(output) => output,
        Err(error) => return ToolResult::fail(error),
    };

    let mut java: Vec<PathBuf> = Vec::new();
    collect_java(&out, &mut java);
    java.sort();

    // 一个文件都没出来才是真失败；jadx 对个别类报错但产出其余部分很常见。
    if java.is_empty() && !output.ok() {
        let tail = output.tail_text(600);
        // `--single-class` 只认**完全限定类名**，短名它直接报 "Input class not found"
        // 然后退出 1。这是最容易踩的一脚：模型手里通常只有 `HomeGuardService`
        // 这种短名，照 schema 里的“类名”字面理解就会传短名。
        if tail.contains("class not found") {
            let wanted = args
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            return ToolResult::fail(format!(
                "jadx 找不到类「{wanted}」：`class` 必须写完全限定类名（带包名，\
                 如 app.orchard.tv.guard.HomeGuardService），短名它不认。\n\
                 不确定全名就先调 bin.dex.disasm（只填 path、不填 class）拿类名清单。"
            ));
        }
        return ToolResult::fail(format!(
            "jadx 失败（退出码 {}）：{}\n样本没有被改动。",
            output.code.unwrap_or(-1),
            tail
        ));
    }

    let mut text = format!(
        "反编译完成：{} 个 java 文件，输出在 `_out/jadx/`（{}）\n",
        java.len(),
        out.display()
    );
    if !output.ok() {
        text.push_str(&format!(
            "注意：退出码 {}，部分类可能没出来：{}\n",
            output.code.unwrap_or(-1),
            output.tail_text(200)
        ));
    }
    text.push_str("\n类文件（前若干条，完整清单在目录里）：\n");
    for path in java.iter().take(PREVIEW) {
        let rel = path.strip_prefix(&out).unwrap_or(path);
        text.push_str(&format!("  {}\n", rel.display()));
    }
    if java.len() > PREVIEW {
        text.push_str(&format!("  …还有 {} 个\n", java.len() - PREVIEW));
    }
    text.push_str("\n用 read_file 读具体文件；路径层级就是包名。");

    ToolResult::plain(ToolOutcome::ok(text))
}

fn collect_java(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_java(&path, out);
        } else if path.extension().map(|ext| ext == "java").unwrap_or(false) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真样本端到端：反编译一个类，产物必须落到 `_out/jadx/` 且文件名对得上。
    ///
    /// `cargo test --lib -- --ignored jadx_decompiles --nocapture`
    #[test]
    #[ignore]
    fn jadx_decompiles_the_sample_apk() {
        use crate::engines::native::tools::{run_with_args, EnabledTools, ToolContext};

        let dir = "/Users/edwa2d4th/Desktop/未命名文件夹 7";
        let apk = format!("{dir}/aerial-guard.apk");
        assert!(
            Path::new(&apk).exists(),
            "样本不在 {apk}：换样本后请同步改这里"
        );
        // 探测结果是排障的第一手信息：路径来自工具链目录 / PATH / 环境变量，
        // 决定了"下次装不起来该看哪儿"。`--nocapture` 就能看到这行。
        if let Ok(status) = crate::engines::native::backends::require("jadx") {
            println!("jadx 探测：{:?}  路径={:?}", status.state, status.path);
        }

        let enabled = EnabledTools::all();
        let stop = || false;
        let ctx = ToolContext {
            app: None,
            session_id: "jadx-test",
            directory: dir,
            call_id: "c1",
            provider: None,
            model: None,
            permission: Some("read-only"),
            enabled: &enabled,
            dag: None,
            should_stop: &stop,
        };

        let result = run_with_args(
            &ctx,
            "bin.jadx.decompile",
            // `--single-class` 要完全限定类名，短名 jadx 会报 "class not found"。
            &json!({ "path": "aerial-guard.apk", "class": "app.orchard.tv.guard.HomeGuardService" }),
        );
        assert!(
            result.outcome.error.is_none(),
            "反编译失败：{:?}",
            result.outcome.error
        );
        println!("{}", result.outcome.text);

        let out = Path::new(dir).join("_out").join("jadx");
        let mut files = Vec::new();
        collect_java(&out, &mut files);
        assert!(!files.is_empty(), "{} 下没有 java 产物", out.display());
        assert!(
            files.iter().any(|path| path
                .file_name()
                .map(|name| name.to_string_lossy().contains("HomeGuardService"))
                .unwrap_or(false)),
            "没生成 HomeGuardService.java（产物 {} 个）",
            files.len()
        );
    }
}
