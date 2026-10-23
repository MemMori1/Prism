# 脚本运行时与安全闸门

> 一句话：**跑脚本要有自己的环境，删东西要能找回来，危险的先拦住。**
> 代码在 `src-tauri/src/engines/native/{guard,runtime}.rs`。

以前 agent 跑代码只有一条路：`run_command` → 宿主的 `sh -c`，工作目录是会话目录，
其余什么都不管。这等于裸奔：`rm -rf ~/Desktop` 照删、`cat ~/.ssh/id_rsa` 照读、
`curl … | sh` 照跑，而模型看到的 `env` 里还有你的 `OPENAI_API_KEY`。

现在分两层，各自解决不同问题：

| | 策略层（`guard`） | 隔离层（`runtime`） |
|---|---|---|
| 解决 | 「这件事该不该做」 | 「做了也跑不出这个圈」 |
| 手段 | 命令文本分析、路径保护、环境清洗、回收站、批量上限 | macOS seatbelt（写入范围 + 禁网） |
| 平台 | 三平台一致 | macOS 有；Windows 只有策略层（`sandbox-exec` 无等价物） |
| 失败时 | 拒绝并说明原因 | **降级并明说「沙箱没生效」** |

---

## 1. 危险命令拦截（`guard::inspect`）

把命令按 `;` `&&` `||` `|` 换行切段，逐段判，产出 `Allow` / `Ask` / `Block`：

**Block（不可逆，直接拒）**
- 递归删根 / 删家目录：`rm -rf /`、`rm -rf ~`、`rm -rf $HOME`、`rm -rf .`
- 磁盘级：`mkfs*`、`diskutil eraseDisk|eraseVolume|reformat`、`dd of=/dev/…`、
  `fdisk`、`parted`、`diskpart`、`format c:`
- 受保护目录内的删除：`rm -rf ~/Desktop/x` → 拒，并指路去 `trash`（可恢复）
- fork bomb、一次删超过 10 个目录

**Ask（危险但可能正当，走审批）**
- 提权：`sudo` / `doas` / `su` / `runas`（看的是段首那个词——`base_of` 会跳过包装命令）
- 抓网即执行：`curl … | sh` / `wget -qO- … | bash`（管道跨段判，见下）
- 混淆执行：`powershell -EncodedCommand`、`eval`、`base64 -d | sh`
- 读凭据：`cat ~/.ssh/id_rsa`、`.env`、`*.pem`、`security`、`defaults`
- 改权限：`chmod` / `chown` / `icacls` / `takeown`
- 往受保护目录写：`cp x ~/Documents/`

**Allow 但留一句 note**：读受保护目录（`ls ~/Documents`）——「扫描=只读」是允许的，
但要在结果里标一句「这儿是只读区」。

两个实现上的坑，都写了注释和测试：
1. 管道在切段之后就没了，`curl … | sh` 变成两段，段内再也看不到 `|` →
   另有一次 `check_pipeline()` 在整条命令上判。
2. `sudo rm -rf /` 的 `base_of` 会跳过 `sudo` 直接看 `rm`，于是「提权」这条规则
   永远不命中 → 提权检查单独看段首词。

## 2. 个人目录强保护

受保护（相对家目录）：`Desktop` `Downloads` `Documents` `Pictures` `Movies`
`Music` `Library` `桌面` `下载` `文稿` `图片` 等，外加 `.ssh` `.aws` `.gnupg`
`.netrc` `.kube` `.docker` `.npmrc`；Windows 另加 `C:\Windows` `C:\Program Files`，
macOS 另加 `/System` `/Library` `/Applications`。家目录本身也算受保护。

规则：
- **读**：允许（扫描只读），但会附一句 note；
- **写**：Ask（要人确认这是你要的位置）；
- **删**：Block，必须用 `trash`。

## 3. 删除走回收站（`guard::trash`）

「删除」不是删除，是**移动到** `~/.prism/trash/<时间戳>/`：

- 每项都写进同目录的 `manifest.json`（`from` → `to`），想找回按 `from` 挪回去；
- 一次最多 10 项（`MAX_BATCH`），多了就分批；
- 拒绝把 `/` 或家目录本身塞进来；
- 跨设备 `rename` 失败时退化成「复制 + 删原件」，宁可多留一份也不丢。

`write_file` 覆盖已有文件前也会先备份到 `~/.prism/backups/<时间戳>/`
（只备份非空且 ≤ 5 MB 的普通文件），结果里写明备份路径。

## 4. 环境清洗（`guard::sanitized_env`）

子进程 `env_clear()` 之后只给白名单：`PATH` `HOME` `LANG` `LC_*` `TERM`
`TMPDIR` `SHELL` `USER` `SYSTEMROOT` `COMSPEC` `PATHEXT` …，并且**再排除**任何
看起来像密钥的：

- 名字含 `KEY` / `TOKEN` / `SECRET` / `PASSWORD` / `CREDENTIAL` / `COOKIE` / `SESSION`
- 前缀 `AWS_` `OPENAI_` `ANTHROPIC_` `GITHUB_` `GITLAB_` `NPM_` `PG` `MYSQL_`
- 注入类：`LD_PRELOAD` `DYLD_*` `BASH_ENV` `ENV` `PYTHONSTARTUP` `NODE_OPTIONS`
  `SSH_AUTH_SOCK`

理由很直接：`env` 一执行，所有密钥就都在上下文里了。

## 5. 脚本运行时（`run_script`）

模型不再需要「先 `write_file` 落脚本，再 `run_command` 跑它」：

```
run_script(language: python|node|sh|powershell, code: "...", purpose: "算样本熵")
```

- 脚本写到 `~/.prism/sandbox/<毫秒-序号>/script.<ext>`（序号防同毫秒并行覆盖）；
- 解释器在 PATH 里探测（`python3`→`python`、`node`、`sh`、`pwsh`→`powershell`）；
  找不到就明说「这台机器上没有 X 的解释器」，而不是报一个 shell 的 127；
- 超时默认 60 秒（上限 600），超时/停止都连进程组一起杀；
- 输出上限 2 万字符；沙箱目录只留最近 20 次（旧的自动清掉，免得攒一堆小文件）；
- 结果头是事实：`沙箱：seatbelt（写入仅限工作目录与临时目录；网络：关）· 解释器 /usr/bin/python3`。

### macOS 的隔离：seatbelt

`seatbelt_profile()` 生成内联 profile（纯函数，有单测）：

```
(version 1)
(allow default)
(deny network*)                     ; 默认禁网
(deny file-write*)                  ; 先全禁写
(allow file-write*                  ; 再放开：工作目录、沙箱目录、临时目录、几个 /dev
  (subpath "<workspace>") (subpath "<sandbox>") (subpath "/private/tmp") …)
(deny file-read*                    ; 个人目录连读都挡掉
  (subpath "<home>/Desktop") (subpath "<home>/Documents") (subpath "<home>/.ssh") …)
```

顺序有意义：seatbelt 是**最后一条匹配的规则生效**，所以 `deny` 在前、`allow` 在后。

`network: true` 会去掉 `(deny network*)`，但它本身要过一次审批（见下）。

### 申请失败就降级，但要如实说

某些环境（已有外层沙箱、MDM 策略）不允许再套一层，`sandbox-exec` 会返回
`sandbox_apply: Operation not permitted`。这时**退回策略层重跑一次**，并在结果头写：

> 沙箱没能应用（sandbox-exec: sandbox_apply: Operation not permitted）：
> 已退回策略层执行——环境仍是清洗过的，但没有 OS 级隔离。

悄悄降级比没有沙箱更糟：用户会以为自己被隔离了。

### Windows

没有 `sandbox-exec` 的等价物（AppContainer / Job Object 需要独立实现，本轮没做）。
所以 Windows 上：**策略层照旧**（命令检测、受保护路径、环境清洗、trash），
隔离只到「工作目录 + 清洗过的环境」，并且结果头明写「无 OS 级隔离」。不假装。

## 6. 沙箱权限开关

两个开关都要**逐次**人点，走的是与工具本身同一个审批闸门
（`approval::gate`，所以「只读」档直接拒、「完全访问」档直接放行）：

| 参数 | 动作 | 卡片标题 |
|---|---|---|
| `dangerouslyDisableSandbox: true` | `unsandbox` | 关掉沙箱跑脚本（以你的身份直接执行） |
| `network: true` | `network` | 允许脚本联网（数据可能出网） |
| 危险命令（Ask 命中） | `danger` | 这条命令有风险：<原因> |

`run_command` 也接受 `dangerouslyDisableSandbox`（它本来就没有沙箱，但这句话本身
不该被模型当成默认组合），同样要一次单独点头。

审批卡上「总是允许」按 **会话 + 动作** 记，进程内有效；重开应用会重新问。

## 7. 行为与 Skill 检测（`guard::scan_text`）

技能、命令、agents 定义都是**外部内容**——写的人不受我们约束。`scan_text` 扫三类：

1. **提示词注入**：「ignore previous instructions」「忽略之前」「你现在是」「you are now」
2. **外传意图**：`id_rsa`、`.aws/credentials`、`.env`、`keychain`、`curl -X POST`、
   `nc -e`、`/dev/tcp/`（反弹 shell）
3. **技能里的命令**：把 ``` 代码块抽出来，按命令规则再扫一遍（`skill-command`）；
   命中 Block 级的（比如 `rm -rf ~`）会直接标成 block。

接入点：工坊读资产（`workshop_read`）时扫，结果放在 `findings` 里，前端在预览顶部
显示一张警示卡（有 block 用红、只有 ask 用琥珀）。

**目前没有「加载技能进会话」这条路**（原生引擎不加载外部技能），所以拦截点就是
「读之前先告诉你」。以后真做技能加载，门槛应该是：`scan_text` 里没有 `block` 命中，
且 ask 命中需要人确认——`guard::scan_text` 已经能直接复用。

## 8. 已知边界（写清楚，别假装）

- **Windows 没有 OS 级隔离**：只有策略层。要真隔离得用 AppContainer /
  Job Object + 只读目录视图，那是另一个工程量。
- **策略层是启发式**：`sh` 的语法比正则复杂，`bash -c "$(echo cm0gLXJmIH4= | base64 -d)"`
  这种绕法能过。它不是安全边界，是**刹车**——主要拦住模型的「顺手一删」。
- **seatbelt 已弃用**：Apple 标了 deprecated（仍可用）。将来换 sandbox profile 的
  正式 API 时只改 `runtime::seatbelt_profile` 与 `build_command` 两处。
- **`trash` 是 Prism 自己的回收站**，不进系统废纸篓（跨平台差异太大）。
- 沙箱里禁读个人目录，**只影响 `run_script`**；`run_command` 没有这层，它靠策略层。

## 9. 测过什么

`cargo test --lib`：新增 21 条，覆盖

- 删根 / 删家目录 / 磁盘格式化 → block；
- 受保护目录内删除 → block 且提示走回收站；
- 管道执行、提权、编码执行 → ask；
- 读凭据 → ask；普通命令（`cargo test` / `ls` / `python3 x.py`）→ allow；
- 引号里的 `;` 不被当分隔符；
- 环境清洗：`OPENAI_API_KEY` 不进子进程（真跑一段 shell 验证）；
- seatbelt profile：deny 在 allow 之前、禁网、引号转义；
- 脚本超时被杀；沙箱申请失败自动降级；
- Skill 文本：注入 + 外传 + 危险命令都能命中，干净文本零命中。
