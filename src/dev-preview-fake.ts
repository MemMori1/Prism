/**
 * 预览用的假 Tauri 后端：把 `invoke` 截下来按命令名返回样例数据。
 *
 * 目的：在真浏览器里跑真 App，验证顶栏与页面行为，不用真的建会话。
 * 场景由 URL `?state=` 选：bound / fresh / legacy。
 *
 * 三条经验坑（上几轮踩过，保留）：
 * 1) 每个 invoke 都要返回**新对象**——共享引用会让组件「自己变」且 watch 不触发。
 * 2) 必须在 App 的模块初始化之前铺好 localStorage：workspace / startPrefs 在 import 时就读。
 * 3) 用另一个端口（1521）起，避免污染真 app（1520 同源）的存储。
 *
 * 维护约定：真后端新增 `invoke` 命令后，这里要补一条，否则预览里会拿到
 * `null` 并在组件里炸成 `Cannot read properties of null`。这个文件在
 * `tsconfig.include` 范围内，`pnpm build` 的类型检查会覆盖它。
 */
import type {
  AgentProfile,
  ArtifactRef,
  DagNode,
  DagPlan,
  DagRun,
  DagSummary,
  PluginInfo,
  SessionSummary,
} from './types'

const params = new URLSearchParams(location.search)
const scene = params.get('state') ?? 'bound'

const now = Date.now()
const MIN = 60_000

function summary(
  engine: string,
  id: string,
  projectId: string,
  title: string,
  ageMs: number,
  directory: string,
): SessionSummary {
  return {
    engine,
    id,
    projectId,
    title,
    directory,
    model: engine === 'native' ? 'deepseek/deepseek-flash' : null,
    cost: 0.0123,
    tokensInput: 1200,
    tokensOutput: 340,
    tokensReasoning: 0,
    timeCreated: now - ageMs - 60_000,
    timeUpdated: now - ageMs,
  }
}

/** 空会话：建出来没跑过，token 与成本都是零——列表底部会提示可以一键关掉。 */
function blankSummary(
  engine: string,
  id: string,
  projectId: string,
  ageMs: number,
  directory: string,
): SessionSummary {
  return {
    ...summary(engine, id, projectId, '新建会话', ageMs, directory),
    cost: 0,
    tokensInput: 0,
    tokensOutput: 0,
    tokensReasoning: 0,
  }
}

const NATIVE_DIR = '/Users/example/hsbg-helper'
const PRISM_DIR = '/Users/example/prism'

/* ---- Agent 注册表 / DAG / 插件：样例数据 ----
 * 真后端加命令时这里必须同步，否则预览里会拿到 null 并在组件里炸。 */

const fakeAgents: AgentProfile[] = [
  {
    id: 'explore',
    name: '探索者',
    description: '只读调研：定位文件、梳理调用链，结论带「文件:行号」。',
    origin: 'builtin',
    overridesBuiltin: false,
    executor: 'native',
    model: null,
    systemPrompt: '你负责只读调研。先搜再读，不修改任何文件。',
    permission: 'read-only',
    workspace: 'shared',
    tools: { shared: ['read_file', 'grep', 'list_files'], private: [], deny: [] },
    mcp: [],
    tags: ['调研'],
    linkDirs: [],
    gateDefault: false,
  },
  {
    id: 'general',
    name: '通用工程师',
    description: '能读能写能跑命令，按任务描述独立完成一个节点。',
    origin: 'builtin',
    overridesBuiltin: false,
    executor: 'native',
    model: null,
    systemPrompt: '你负责把一个节点的任务做完，输出结论与改动清单。',
    permission: 'workspace-write',
    workspace: 'worktree',
    tools: { shared: null, private: [], deny: [] },
    mcp: [],
    tags: ['实现'],
    linkDirs: [],
    gateDefault: false,
  },
  {
    id: 'frontend',
    name: '前端工程师',
    description: '前端实现：组件、样式、交互，保持现有设计令牌与间距体系。',
    origin: 'builtin',
    overridesBuiltin: false,
    executor: 'native',
    model: null,
    systemPrompt: '你负责前端实现。先读再改，不引入新组件库。',
    permission: 'workspace-write',
    workspace: 'worktree',
    tools: { shared: ['read_file', 'edit_file', 'run_command'], private: [], deny: [] },
    mcp: [],
    tags: ['前端', '实现'],
    linkDirs: ['node_modules'],
    gateDefault: false,
  },
  {
    id: 'code-reviewer',
    name: '代码审查',
    description: '自定义：按团队规范审查 diff，输出分级问题清单。',
    origin: 'user',
    overridesBuiltin: false,
    executor: 'native',
    model: null,
    systemPrompt: '按团队规范审查代码：先看意图，再看边界与错误处理。',
    permission: 'read-only',
    workspace: 'shared',
    tools: { shared: ['read_file', 'grep'], private: [], deny: [] },
    mcp: [],
    tags: ['审查'],
    linkDirs: [],
    gateDefault: false,
  },
]

function fakeNode(
  id: string,
  title: string,
  task: string,
  dependsOn: string[],
  agent: string | null,
  extra: Partial<DagNode> = {},
): DagNode {
  return {
    id,
    title,
    task,
    dependsOn,
    agent,
    workspace: null,
    tools: null,
    permission: null,
    retry: { attempts: 0, backoffMs: 2000 },
    timeoutSecs: null,
    gate: null,
    position: null,
    inputArtifacts: [],
    outputArtifactKey: '',
    ...extra,
  }
}

function fakePlan(id = 'dag-20261022-120400001'): DagPlan {
  return {
    version: 1,
    id,
    name: '给设置页加自动更新',
    goal: '在设置里加自动更新，包含检查 / 下载 / 重启',
    project: PRISM_DIR,
    defaults: {
      agent: 'general',
      permission: 'workspace-write',
      maxParallel: 2,
      failurePolicy: 'fail-fast',
      tokenBudget: null,
    },
    nodes: [
      fakeNode('research', '摸清更新链路', '调查 Tauri updater 的接线与缺口，输出「文件:行号」。', [], 'explore', {
        workspace: 'shared',
      }),
      fakeNode(
        'frontend',
        '设置页 UI',
        '实现检查 / 下载 / 重启交互。\n{{nodes.research.output}}',
        ['research'],
        'frontend',
        { workspace: 'worktree' },
      ),
      fakeNode('backend', 'IPC 与更新流程', '接 updater 的三个命令与事件。', ['research'], null, {
        workspace: 'worktree',
      }),
      fakeNode('verify', '合并验证', '合上游改动，跑构建与测试。', ['frontend', 'backend'], null, {
        workspace: 'worktree',
        gate: { before: true, after: false },
      }),
    ],
    layout: {},
    createdAt: now - 3 * MIN,
    updatedAt: now - MIN,
  }
}

const fakePlugins: PluginInfo[] = [
  { id: 'engine.native', requires: [], state: 'active', reason: null },
  { id: 'engine.opencode', requires: [], state: 'active', reason: null },
  { id: 'engine.codex', requires: [], state: 'active', reason: null },
  { id: 'engine.dsh', requires: [], state: 'active', reason: null },
  { id: 'engine.claude', requires: [], state: 'active', reason: null },
  { id: 'engine.host', requires: [], state: 'active', reason: null },
  { id: 'tray', requires: [], state: 'active', reason: null },
  { id: 'quota', requires: [], state: 'active', reason: null },
  { id: 'tasks', requires: [], state: 'active', reason: null },
  { id: 'routines', requires: ['engine.host'], state: 'active', reason: null },
  { id: 'tools.mcp', requires: [], state: 'active', reason: null },
  { id: 'dag.executor.native', requires: ['engine.native'], state: 'active', reason: null },
  { id: 'dag.executor.codex', requires: ['engine.codex'], state: 'active', reason: null },
  { id: 'dag.executor.dsh', requires: ['engine.dsh'], state: 'active', reason: null },
  { id: 'dag.executor.opencode', requires: ['engine.opencode'], state: 'active', reason: null },
]

let fakePlans: DagPlan[] = [fakePlan()]
let fakeRun: DagRun | null = null

const projects = [
  {
    engine: 'native',
    id: `native:${NATIVE_DIR}`,
    worktree: NATIVE_DIR,
    name: 'hsbg-helper',
    sessionCount: 2,
  },
  {
    engine: 'opencode',
    id: `opencode:${NATIVE_DIR}`,
    worktree: NATIVE_DIR,
    name: 'hsbg-helper',
    sessionCount: 1,
  },
  {
    engine: 'codex',
    id: `codex:${PRISM_DIR}`,
    worktree: PRISM_DIR,
    name: 'Prism',
    sessionCount: 1,
  },
  {
    // 故意与上面同名不同目录：验证下拉用父级路径区分重名。
    engine: 'native',
    id: 'native:/Users/example/work/hsbg-helper',
    worktree: '/Users/example/work/hsbg-helper',
    name: 'hsbg-helper',
    sessionCount: 0,
  },
]

const store: Record<string, SessionSummary[]> = {
  [`native:${NATIVE_DIR}`]: [
    summary('native', 'native:n1', `native:${NATIVE_DIR}`, '修 README 的安装步骤', 3 * MIN, NATIVE_DIR),
    summary('native', 'native:n2', `native:${NATIVE_DIR}`, '顶部导航', 20 * MIN, NATIVE_DIR),
    // 两个空会话：预览里用来验「一键关闭空会话」那条收拾条与恢复。
    blankSummary('native', 'native:b1', `native:${NATIVE_DIR}`, 45 * MIN, NATIVE_DIR),
    blankSummary('native', 'native:b2', `native:${NATIVE_DIR}`, 70 * MIN, NATIVE_DIR),
  ],
  [`opencode:${NATIVE_DIR}`]: [
    summary('opencode', 'oc1', `opencode:${NATIVE_DIR}`, '加个导出按钮', 6 * MIN, NATIVE_DIR),
  ],
  [`codex:${PRISM_DIR}`]: [
    summary('codex', 'cx1', `codex:${PRISM_DIR}`, '打包体积', 30 * MIN, PRISM_DIR),
  ],
}

/** 后台任务演示数据（M23）：任务坞 / 终端页在预览里也能看到内容。 */
let taskSeq = 0
const firstSessionId = Object.values(store)[0]?.[0]?.id ?? null
const taskNow = Date.now()
const taskStore: Array<{
  id: string
  cmd: string
  cwd: string
  state: 'running' | 'done' | 'failed' | 'stopped'
  startedAt: number
  endedAt: number | null
  exitCode: number | null
  sessionId: string | null
  title: string | null
  background: boolean
  origin: 'user' | 'agent'
  tail: string[]
}> = []

function seedTasks() {
  if (taskStore.length) return
  taskStore.push(
    {
      id: 'task-fake-agent',
      cmd: 'pnpm -r test --filter desktop',
      cwd: NATIVE_DIR,
      state: 'running',
      startedAt: taskNow - 22_000,
      endedAt: null,
      exitCode: null,
      sessionId: firstSessionId,
      title: null,
      background: true,
      origin: 'agent',
      tail: [
        '$ pnpm -r test --filter desktop',
        'apps/desktop test$ vitest run',
        '✓ tests/dock.test.ts (12 tests) 214ms',
        '· running tests/terminal.test.ts…',
      ],
    },
    {
      id: 'task-fake-agent-done',
      cmd: 'cargo check --quiet',
      cwd: NATIVE_DIR,
      state: 'done',
      startedAt: taskNow - 180_000,
      endedAt: taskNow - 168_000,
      exitCode: 0,
      sessionId: firstSessionId,
      title: null,
      background: true,
      origin: 'agent',
      tail: ['$ cargo check --quiet', '✓ 0 errors · 11.2s'],
    },
    {
      id: 'task-fake-build',
      cmd: 'pnpm build',
      cwd: NATIVE_DIR,
      state: 'running',
      startedAt: taskNow - 72_000,
      endedAt: null,
      exitCode: null,
      sessionId: firstSessionId,
      title: null,
      background: true,
      origin: 'user',
      tail: [
        '$ pnpm build',
        'scope: all 2 workspace projects',
        'packages/web build$ vite build',
        'vite v6.0.7 building for production...',
        '✓ 412 modules transformed.',
        'dist/assets/index-Bq7x.css   38.42 kB',
        'apps/desktop build$ tauri build',
        '   Compiling prism v0.2.3 (/apps/desktop)',
        '   Finished `release` profile [optimized] in 11.4s',
        'warning: unused manifest key: bundle.shortDescription',
        '· bundling dmg (aarch64)…',
      ],
    },
    {
      id: 'task-fake-test',
      cmd: 'vitest run tests/composer',
      cwd: NATIVE_DIR,
      state: 'running',
      startedAt: taskNow - 38_000,
      endedAt: null,
      exitCode: null,
      sessionId: firstSessionId,
      title: null,
      background: true,
      origin: 'user',
      tail: [
        '$ vitest run tests/composer',
        'RUN  v5.0.5',
        '✓ tests/composer.test.ts (18 tests) 412ms',
        '· waiting for worker to settle…',
      ],
    },
    {
      id: 'task-fake-lint',
      cmd: 'npm run lint',
      cwd: NATIVE_DIR,
      state: 'done',
      startedAt: taskNow - 240_000,
      endedAt: taskNow - 120_000,
      exitCode: 0,
      sessionId: firstSessionId,
      title: null,
      background: true,
      origin: 'user',
      tail: ['$ npm run lint', 'eslint . --max-warnings 0', '✓ 0 errors, 0 warnings · 12.1s'],
    },
    {
      id: 'task-fake-push',
      cmd: 'git push origin feat/background-dock',
      cwd: NATIVE_DIR,
      state: 'failed',
      startedAt: taskNow - 320_000,
      endedAt: taskNow - 300_000,
      exitCode: 128,
      sessionId: firstSessionId,
      title: null,
      background: true,
      origin: 'user',
      tail: [
        '$ git push origin feat/background-dock',
        'remote: error: GH006 protected branch update failed',
        '✕ exited with code 128',
      ],
    },
  )
}

/** 时间线：每个会话一份记录（read_timeline 每次返回深拷贝）。 */
const timelines: Record<string, Array<Record<string, unknown>>> = {}
let seq = 1000

function pushRecord(sessionId: string, role: 'user' | 'assistant', text: string) {
  seq += 1
  const list = timelines[sessionId] ?? (timelines[sessionId] = [])
  list.push({
    id: `m${seq}`,
    role,
    timeCreated: Date.now(),
    model: role === 'assistant' ? 'deepseek-chat' : null,
    completed: true,
    parts: [{ kind: 'text', text }],
    tokens: null,
  })
}

pushRecord('native:n1', 'user', '把 README 的安装步骤补全')
pushRecord('native:n1', 'assistant', '好，我看了 README：现在缺 macOS 与 Windows 两段。')
// 临时：给最后一轮塞 token，用来验上下文圆环（验完删）
if (scene === 'ctx') {
  const list = timelines['native:n1']
  const last = list[list.length - 1]
  last.tokens = { input: 10246, cacheRead: 0, cacheWrite: 0, output: 174, reasoning: 0 }
}
if (scene === 'json') {
  timelines['native:n1'].push({
    id: 'm-tool',
    role: 'assistant',
    timeCreated: Date.now(),
    model: 'deepseek-chat',
    completed: true,
    parts: [
      {
        kind: 'text',
        text: '我把 `src/api.ts` 里的调用补上了，`README.md` 也加了一行；顺手跑了 `npm run build`。',
      },
      { kind: 'tool', name: 'edit', status: 'completed', summary: 'src/api.ts', callId: 'call_fake_1' },
    ],
    tokens: null,
  })
  pushRecord(
    'native:n1',
    'assistant',
    '配置是这样（`prism.json`）：\n\n```json\n{\n  "engine": "native",\n  "providers": [\n    { "id": "deepseek", "baseUrl": "https://api.deepseek.com/v1", "models": ["deepseek-chat", "deepseek-reasoner"] }\n  ],\n  "maxTurns": 12,\n  "stream": true,\n  "budget": null,\n  "ratio": 0.85\n}\n```\n\nkey 换成你自己的再跑。',
  )
}
// 子代理：验证工具卡的「子代理过程」步骤轨迹与报告区
if (scene === 'subagent') {
  timelines['native:n1'].push({
    id: 'm-task',
    role: 'assistant',
    timeCreated: Date.now(),
    model: 'deepseek-chat',
    completed: true,
    parts: [
      {
        kind: 'text',
        text: '我让探索子代理把登录链路摸了一遍，结论如下：入口在 `src/auth/login.ts`，令牌刷新散落在两个文件里。',
      },
      {
        kind: 'tool',
        name: 'task',
        status: 'completed',
        summary: '梳理登录与令牌刷新链路',
        callId: 'call_fake_task',
      },
    ],
    tokens: { input: 4210, cacheRead: 0, cacheWrite: 0, output: 388, reasoning: 0 },
  })
}
// 长会话：验证「进来就贴到底」（一屏放不下才测得出滚动位置）
if (scene === 'long') {
  const fence = '```'
  for (let index = 0; index < 26; index += 1) {
    pushRecord('native:n1', 'user', '第 ' + (index + 1) + ' 个问题：帮我看看这段代码为什么不对')
    pushRecord(
      'native:n1',
      'assistant',
      '这是第 ' + (index + 1) + ' 条回复。\n\n' + fence + 'ts\nconst value' + index +
        ' = ' + index + '\nexport function step' + index + '() {\n  return value' + index +
        ' * 2\n}\n' + fence + '\n\n要点：先确认输入，再看边界。',
    )
  }
}
pushRecord('native:n2', 'user', '顶部导航要能新建会话')
pushRecord('native:n2', 'assistant', '明白，我把导航放在 logo 右侧。')
pushRecord('oc1', 'user', '加个导出按钮')
pushRecord('oc1', 'assistant', '导出的格式你想先支持哪个？')
pushRecord('cx1', 'user', '打包体积还能再小点吗')
pushRecord('cx1', 'assistant', '可以用 release profile + strip。')

// ---- localStorage 场景（必须在 App 模块初始化前铺好） ----

function seed(key: string, value: unknown) {
  localStorage.setItem(key, typeof value === 'string' ? value : JSON.stringify(value))
}

localStorage.removeItem('prism-workbench-v1')
localStorage.removeItem('prism-workspace-v1')

// 「最近打开」的样张：面板里开浏览器时，起始页得有两列东西可看。
seed('prism-recent-pages-v1', [
  { url: 'https://doc.rust-lang.org/std/', title: 'std - Rust', at: Date.now() - 8 * MIN },
  { url: 'https://tauri.app/start/', title: 'Tauri 2 · Getting Started', at: Date.now() - 52 * MIN },
  { url: 'http://localhost:1521/dev-preview.html', title: 'Prism 预览', at: Date.now() - 190 * MIN },
])
seed('prism-recent-files-v1', [
  { path: `${NATIVE_DIR}/src/components/DagPane.vue`, directory: NATIVE_DIR, at: Date.now() - 12 * MIN },
  { path: `${NATIVE_DIR}/src-tauri/src/ipc.rs`, directory: NATIVE_DIR, at: Date.now() - 40 * MIN },
  { path: `${PRISM_DIR}/docs/PANEL_BROWSER.md`, directory: PRISM_DIR, at: Date.now() - 95 * MIN },
])
localStorage.setItem('prism-view', scene === 'json' ? 'archive' : 'workbench')

if (scene === 'legacy') {
  // 旧版「单画布 + 列」：应该迁成「一列一页」，并认领回 native。
  seed('prism-workbench-v1', [{ sessionId: 'native:n1', width: 520 }])
} else if (scene === 'fresh') {
  // 两页：p1 是 native（有会话），当前页 p2 是空的新页面（还没挑 Agent）。
  seed('prism-workspace-v1', {
    currentId: 'p2',
    pages: [
      {
        id: 'p1',
        agent: 'native',
        directory: NATIVE_DIR,
        startOpen: true,
        focusedId: null,
        columns: [{ sessionId: 'native:n1', width: 480 }],
      },
      { id: 'p2', agent: null, directory: null, startOpen: true, focusedId: null, columns: [] },
    ],
  })
} else if (scene === 'gone') {
  // 历史会话记的目录已经不存在了：下拉里该项要标红，草稿框不许发。
  seed('prism-workspace-v1', {
    currentId: 'p1',
    pages: [
      {
        id: 'p1',
        agent: 'native',
        directory: '/Users/example/old-project',
        startOpen: true,
        focusedId: null,
        columns: [{ sessionId: 'native:n1', width: 480 }],
      },
    ],
  })
} else if (scene === 'canvas') {
  // 截图用：直接落在多列画布上（startOpen=false），两列都是 native。
  seed('prism-workspace-v1', {
    currentId: 'p1',
    pages: [
      {
        id: 'p1',
        agent: 'native',
        directory: NATIVE_DIR,
        startOpen: false,
        focusedId: 'native:n1',
        columns: [
          { sessionId: 'native:n1', width: 520 },
          { sessionId: 'native:n2', width: 460 },
        ],
      },
    ],
  })
} else {
  // bound：一页 native，两列。
  seed('prism-workspace-v1', {
    currentId: 'p1',
    pages: [
      {
        id: 'p1',
        agent: 'native',
        directory: NATIVE_DIR,
        startOpen: true,
        focusedId: 'native:n1',
        columns: [
          { sessionId: 'native:n1', width: 480 },
          { sessionId: 'native:n2', width: 480 },
        ],
      },
    ],
  })
}

// ---- 假后端 ----

const annotationStore: {
  version: number
  tags: Array<Record<string, unknown>>
  sessions: Record<
    string,
    { title: string | null; starred: boolean; tags: string[]; hidden?: boolean }
  >
} = { version: 1, tags: [], sessions: {} }


/** 截图用：造一份像样的 28 天用量（首页 / 用量页的曲线与分布都靠它）。 */
function fakeUsage() {
  const tokens = (input: number, output: number, reasoning: number, cacheRead: number, cacheWrite: number) => ({
    input,
    output,
    reasoning,
    cacheRead,
    cacheWrite,
  })
  const today = new Date()
  const daily: Array<{ date: string; tokens: ReturnType<typeof tokens> }> = []
  let totalInput = 0
  let totalOutput = 0
  let totalReasoning = 0
  let totalCacheRead = 0
  let totalCacheWrite = 0
  for (let back = 27; back >= 0; back -= 1) {
    const day = new Date(today)
    day.setDate(today.getDate() - back)
    const weekday = day.getDay()
    const weekend = weekday === 0 || weekday === 6
    // 越近越忙；周末打个折，再叠一点波动，别像一条直线
    const wave = 0.72 + 0.28 * Math.sin((27 - back) / 3.1)
    const scale = (weekend ? 0.35 : 1) * wave
    const input = Math.round((46000 + (27 - back) * 2600) * scale)
    const output = Math.round(input * 0.19)
    const reasoning = Math.round(input * 0.06)
    const cacheRead = Math.round(input * 0.52)
    const cacheWrite = Math.round(input * 0.03)
    totalInput += input
    totalOutput += output
    totalReasoning += reasoning
    totalCacheRead += cacheRead
    totalCacheWrite += cacheWrite
    daily.push({
      date: `${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, '0')}-${String(day.getDate()).padStart(2, '0')}`,
      tokens: tokens(input, output, reasoning, cacheRead, cacheWrite),
    })
  }

  const hours = [
    0, 0, 0, 0, 0, 0, 1, 2, 4, 7, 9, 8, 5, 3, 6, 9, 11, 8, 4, 3, 5, 8, 9, 4,
  ]

  return {
    days: 28,
    sessions: 23,
    totals: tokens(totalInput, totalOutput, totalReasoning, totalCacheRead, totalCacheWrite),
    costUsd: 18.42,
    engines: [
      {
        engine: 'opencode',
        label: 'OpenCode',
        tokens: tokens(412_000, 78_000, 21_000, 268_000, 9_600),
        sessions: 9,
        costUsd: 9.86,
        unpricedModels: [],
        models: [
          { model: 'claude-sonnet-4.5', tokens: tokens(268_000, 51_000, 12_000, 174_000, 6_100), sessions: 6, costUsd: 6.42 },
          { model: 'gpt-5-codex', tokens: tokens(144_000, 27_000, 9_000, 94_000, 3_500), sessions: 3, costUsd: 3.44 },
        ],
      },
      {
        engine: 'codex',
        label: 'Codex',
        tokens: tokens(286_000, 64_000, 38_000, 196_000, 7_200),
        sessions: 7,
        costUsd: 5.31,
        unpricedModels: [],
        models: [
          { model: 'gpt-5.1-codex', tokens: tokens(286_000, 64_000, 38_000, 196_000, 7_200), sessions: 7, costUsd: 5.31 },
        ],
      },
      {
        engine: 'claude',
        label: 'Claude Code',
        tokens: tokens(148_000, 31_000, 6_000, 88_000, 2_100),
        sessions: 4,
        costUsd: 2.18,
        unpricedModels: [],
        models: [
          { model: 'claude-opus-4.1', tokens: tokens(148_000, 31_000, 6_000, 88_000, 2_100), sessions: 4, costUsd: 2.18 },
        ],
      },
      {
        engine: 'native',
        label: 'Prism 原生',
        tokens: tokens(96_000, 24_000, 11_000, 61_000, 1_800),
        sessions: 3,
        costUsd: 1.07,
        unpricedModels: [],
        models: [
          { model: 'deepseek-chat', tokens: tokens(96_000, 24_000, 11_000, 61_000, 1_800), sessions: 3, costUsd: 1.07 },
        ],
      },
    ],
    daily,
    hours,
    notes: [],
  }
}

const calls: Record<string, number> = {}
const api = { calls }
;(window as unknown as { __calls: Record<string, number>; __api: unknown }).__calls = calls
;(window as unknown as { __api: unknown }).__api = api

let created = 0
let eventId = 0

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>

const eventListeners = new Set<{ event: string; fn: (envelope: unknown) => void }>()

/* ---- DAG 运行模拟 ----
 * 预览里没有真模型，所以按依赖手动推进状态机：每个 tick 收掉在跑的、放下
 * 依赖已齐的，再用 `dag.run` 事件推出去——和真后端推的载荷是同一个形状。 */

const FAKE_STEPS: Record<string, string[]> = {
  research: ['grep "updater" src-tauri', 'read_file src-tauri/Cargo.toml', 'read_file src/lib.rs'],
  frontend: ['read_file src/components/SettingsDialog.vue', 'edit_file src/components/SettingsDialog.vue'],
  backend: ['read_file src-tauri/src/ipc.rs', 'edit_file src-tauri/src/ipc.rs'],
  verify: ['run_command pnpm build', 'run_command cargo test --lib'],
}

let runTimer = 0

/** 假制品表：预览里点「查看全文」要能看见东西，否则面板是空的。 */
const fakeArtifacts: ArtifactRef[] = []
const fakeArtifactText = new Map<string, string>()
let artifactSeq = 0

function makeArtifact(dagId: string, nodeId: string, key: string, text: string): ArtifactRef {
  artifactSeq += 1
  const id = `art-20261022-${String(artifactSeq).padStart(6, '0')}`
  const artifact: ArtifactRef = {
    id,
    dagId,
    nodeId,
    key,
    kind: 'text',
    name: `${key}.md`,
    size: new TextEncoder().encode(text).length,
    // 假哈希：只要位数像就行，界面只显示前 12 位。
    sha256: `9f2c${String(artifactSeq).padStart(4, '0')}4b7e1a8d3c5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9`,
    parentId: null,
    createdAt: Date.now(),
  }
  fakeArtifacts.unshift(artifact)
  fakeArtifactText.set(id, text)
  return artifact
}

function emitRun(run: DagRun): void {
  for (const listener of eventListeners) {
    if (listener.event !== 'dag.run') continue
    listener.fn({ event: 'dag.run', id: 0, payload: JSON.parse(JSON.stringify(run)) })
  }
}

/** 与 Rust 侧 `NodeLive` 同一个形状：预览里也要能看到实时正文与步骤。 */
function emitLive(
  runId: string,
  nodeId: string,
  delta: string,
  step: string | null,
  tin: number,
  tout: number,
  turn: number,
  maxTurns: number,
): void {
  for (const listener of eventListeners) {
    if (listener.event !== 'dag.node.live') continue
    listener.fn({
      event: 'dag.node.live',
      id: 0,
      payload: { runId, nodeId, delta, step, tokensIn: tin, tokensOut: tout, turn, maxTurns },
    })
  }
}

/** 假正文的“逐字”来源：每次推一小段，肉眼看上去像在写。 */
const FAKE_LIVE_SCRIPT = [
  '先看样本的头部结构。',
  '跑一遍节表，确认入口点落在哪一节。',
  '导入表里有几个可疑的 Win32 调用。',
  '继续追字符串引用。',
  '结论：入口在加壳的 stub 上，需要先脱壳再静态分析。',
]

/** 预览里的轮次上限：让「第 3/12 轮」这种文案有东西可显示。 */
const FAKE_TURNS = 12

/** 每个节点在假脚本里的位置（第几句、第几个字）。 */
const fakeLiveCursor: Record<string, { line: number; at: number }> = {}

function finishRun(): void {
  const run = fakeRun
  if (!run) return
  const failed = run.nodes.filter(node => node.status === 'failed').length
  const cancelled = run.nodes.filter(node => node.status === 'cancelled').length
  const skipped = run.nodes.filter(node => node.status === 'skipped').length
  run.status = failed ? 'failed' : cancelled ? 'cancelled' : 'succeeded'
  run.finishedAt = Date.now()
  const parts: string[] = []
  if (failed) parts.push(`失败：${run.nodes.filter(n => n.status === 'failed').map(n => n.id).join('、')}`)
  if (skipped) parts.push(`${skipped} 个节点被跳过`)
  if (cancelled) parts.push(`${cancelled} 个节点被取消`)
  run.message = parts.length ? parts.join('；') : null
  if (runTimer) {
    window.clearInterval(runTimer)
    runTimer = 0
  }
  emitRun(run)
}

function stepRun(): void {
  const run = fakeRun
  if (!run || run.status !== 'running') {
    if (runTimer) {
      window.clearInterval(runTimer)
      runTimer = 0
    }
    return
  }
  const plan = fakePlans.find(item => item.id === run.planId)
  const statusOf = (id: string): string => run.nodes.find(node => node.id === id)?.status ?? 'pending'

  // 1) 上一 tick 放出去的收到成功（让 running 至少可见一帧）
  for (const node of run.nodes) {
    if (node.status !== 'running') continue
    node.status = 'succeeded'
    node.finishedAt = Date.now()
    node.output = [
      `## ${node.title}`,
      '',
      '（预览假数据）这一节点跑完了：读 3 个文件、改 1 个。',
      '',
      '- 入口：`src/App.vue`',
      '- 改动：`src/components/DagPane.vue`',
      '',
      '```ts',
      'const html = renderMarkdown(report)',
      '```',
      '',
      '| 项目 | 值 |',
      '| --- | --- |',
      '| 轮次 | 6 |',
      '| 用时 | 2m14s |',
    ].join('\n')
    node.steps = FAKE_STEPS[node.id] ?? ['read_file src/App.vue']
    // 每个成功节点都落一份制品：面板上能看见 key / 大小 / 哈希 / 引用。
    node.artifact = makeArtifact(run.planId, node.id, node.id === 'research' ? 'report' : 'report', node.output)
    node.tokensIn += 1_800
    node.tokensOut += 420
  }

  // 2) 依赖已齐的 pending 放出去
  const released = run.nodes.filter(node => {
    if (node.status !== 'pending') return false
    const source = plan?.nodes.find(item => item.id === node.id)
    return (source?.dependsOn ?? []).every(dep => statusOf(dep) === 'succeeded')
  })
  const parallel = Math.max(1, plan?.defaults.maxParallel ?? 2)
  let free = parallel - run.nodes.filter(node => node.status === 'running').length
  for (const node of released) {
    const source = plan?.nodes.find(item => item.id === node.id)
    if (source?.gate?.before) {
      // 闸门：先停在待确认，等 `dag_run_approve`
      node.status = 'waiting'
      continue
    }
    if (free <= 0) continue
    node.status = 'running'
    node.attempt += 1
    node.startedAt = Date.now()
    free -= 1
  }

  const busy = run.nodes.some(node => node.status === 'running' || node.status === 'waiting')
  if (!busy) {
    finishRun()
    return
  }
  emitRun(run)

  // 3) 给在跑的节点推增量：正文一小段 + 偶尔一步工具调用。真机上这两个
  //    由执行器的 on_text / on_step 回调逐条推出来，预览要对齐同一个形状。
  for (const node of run.nodes) {
    if (node.status !== 'running') continue
    const cursor = (fakeLiveCursor[node.id] ??= { line: 0, at: 0 })
    const script = FAKE_LIVE_SCRIPT[cursor.line % FAKE_LIVE_SCRIPT.length]
    const take = Math.min(script.length, cursor.at + 4 + (cursor.line % 3))
    let delta = script.slice(cursor.at, take)
    cursor.at = take
    if (take >= script.length) {
      // 一句说完换下一句；光标归零，下一 tick 从新句子开头开始。
      cursor.line += 1
      cursor.at = 0
      delta += '\n'
    }
    const step = cursor.line % 2 === 0 && cursor.at === 0 ? (FAKE_STEPS[node.id]?.[0] ?? 'read_file sample.bin') : null
    node.tokensIn += 220
    node.tokensOut += 16
    // 预览里也把轮次推进起来：真机上这是 on_turn 报的第 N 轮。
    const turn = 1 + (cursor.line % FAKE_TURNS)
    node.turns = turn - 1
    node.maxTurns = FAKE_TURNS
    emitLive(run.id, node.id, delta, step, node.tokensIn, node.tokensOut, turn, FAKE_TURNS)
  }
}

function startRun(plan: DagPlan): DagRun {
  const run: DagRun = {
    id: `run-${plan.id}-${Date.now()}`,
    planId: plan.id,
    planName: plan.name,
    project: plan.project,
    status: 'running',
    startedAt: Date.now(),
    finishedAt: null,
    message: null,
    nodes: plan.nodes.map(node => ({
      id: node.id,
      title: node.title,
      agent: node.agent ?? plan.defaults.agent,
      status: 'pending' as const,
      attempt: 0,
      startedAt: null,
      finishedAt: null,
      output: '',
      error: null,
      tokensIn: 0,
      tokensOut: 0,
      steps: [],
      turns: 0,
      maxTurns: 0,
      artifact: null,
    })),
  }
  fakeRun = run
  emitRun(run)
  if (runTimer) window.clearInterval(runTimer)
  runTimer = window.setInterval(stepRun, 900)
  window.setTimeout(stepRun, 120)
  return run
}

const invoke: Invoke = async (cmd, args = {}) => {
  calls[cmd] = (calls[cmd] ?? 0) + 1
  // 真 listen() 走这个命令：把 handler 记下来，脚本里用 __emitEngine 推事件。
  if (cmd === 'plugin:event|listen') {
    const { handler, event } = args as { handler?: number; event?: string }
    const fn = handler ? (window as unknown as Record<string, unknown>)[`_${handler}`] : null
    if (typeof fn === 'function') {
      // 真 Tauri 按事件名分发；假后端也要过滤，否则 task-state 会被 task-output 的
      // 监听器当成输出行，反过来把 tail 写成 undefined。
      eventListeners.add({ event: String(event ?? ''), fn: fn as (envelope: unknown) => void })
      ;((window as unknown as Record<string, unknown[]>).__listenMeta ??= []).push({
        event,
        handler,
      })
    }
    return eventId
  }
  switch (cmd) {
    case 'engine_health':
      return [
        {
          engineId: 'native',
          displayName: 'Prism 原生',
          databasePath: '/Users/example/.prism/native.db',
          cliPath: null,
          ready: true,
        },
        {
          engineId: 'opencode',
          displayName: 'OpenCode',
          databasePath: '/Users/example/.local/share/opencode/opencode.db',
          cliPath: '/usr/local/bin/opencode',
          ready: true,
        },
        {
          engineId: 'codex',
          displayName: 'Codex',
          databasePath: '/Users/example/.codex/sessions',
          cliPath: '/usr/local/bin/codex',
          ready: true,
        },
        {
          engineId: 'dsh',
          displayName: 'DeepSeek Harness',
          databasePath: '/Users/example/.dsh',
          cliPath: '/usr/local/bin/dsh',
          ready: true,
        },
        {
          engineId: 'claude',
          displayName: 'Claude Code',
          databasePath: null,
          cliPath: null,
          ready: false,
        },
      ]
    case 'list_projects':
      return projects.map(project => ({ ...project }))
    case 'list_sessions': {
      const list = store[String(args.projectId)] ?? []
      return list.map(item => ({ ...item }))
    }
    case 'session_summary': {
      const id = String(args.sessionId)
      for (const list of Object.values(store)) {
        const found = list.find(item => item.id === id)
        if (found) return { ...found }
      }
      return null
    }
    case 'session_create': {
      created += 1
      const engine = String(args.engine ?? 'native')
      const directory = String(args.directory ?? NATIVE_DIR)
      const id = `${engine}:new${created}`
      const projectId = `${engine}:${directory}`
      const bucket = store[projectId] ?? (store[projectId] = [])
      bucket.unshift(summary(engine, id, projectId, null as unknown as string, 0, directory))
      const createdSummary = bucket[0]
      createdSummary.title = null as unknown as string
      return { sessionId: id, directory }
    }
    case 'session_send': {
      const id = String(args.sessionId)
      const text = String(args.text ?? '')
      pushRecord(id, 'user', text)
      pushRecord(id, 'assistant', `收到：「${text}」——我先看一遍相关文件。`)
      return null
    }
    case 'read_timeline': {
      const id = String(args.sessionId)
      const records = (timelines[id] ?? []).map(record => JSON.parse(JSON.stringify(record)))
      return { records, nextCursor: null, hasMore: false }
    }
    case 'read_tool_detail': {
      const callId = String(args.callId ?? '')
      if (callId === 'call_fake_task') {
        return {
          input: {
            description: '梳理登录与令牌刷新链路',
            prompt: '把登录、会话、令牌刷新相关的文件与调用链梳理清楚，报告覆盖入口、存储、刷新时机与风险点。',
            agent_type: 'explore',
          },
          output:
            '结论：登录入口在 `src/auth/login.ts`（38 行），令牌存 `src/auth/store.ts`（localStorage）。\n' +
            '刷新时机有两处：`src/api.ts` 的 401 拦截器（112 行）与 `App.vue` 的 onMounted（27 行）。\n' +
            '风险：两处刷新没有互斥，并发请求可能触发多次 refresh_token。',
          error: null,
          metadata: {
            subagent: {
              agentType: 'explore',
              label: '探索',
              turns: 3,
              steps: [
                { kind: 'text', text: '先搜 auth / token 相关文件，定位入口。' },
                { kind: 'tool', name: 'grep', summary: 'refresh_token', status: 'completed' },
                { kind: 'tool', name: 'read_file', summary: 'src/auth/login.ts', status: 'completed' },
                { kind: 'text', text: '再看刷新时机，有两处。' },
                { kind: 'tool', name: 'read_file', summary: 'src/api.ts', status: 'completed' },
                { kind: 'tool', name: 'read_file', summary: 'src/App.vue', status: 'completed' },
                { kind: 'tool', name: 'grep', summary: 'localStorage', status: 'error' },
              ],
            },
          },
          truncated: false,
        }
      }
      return {
        input: { path: 'src/api.ts' },
        output: 'ok',
        error: null,
        metadata: null,
        truncated: false,
      }
    }
    case 'session_permissions':
      return []
    case 'session_permission_reply':
    case 'session_interrupt':
    case 'session_shell':
    case 'session_command':
    case 'tray_status':
    case 'open_path':
      return null
    case 'project_files': {
      const query = String(args.query ?? '').toLowerCase()
      const list = [
        'prism.json',
        'README.md',
        'package.json',
        'vite.config.ts',
        'src/main.ts',
        'src/api.ts',
        'src/styles/base.css',
        'src/components/SessionView.vue',
        'src/components/ChangesPane.vue',
        'src/components/FilePane.vue',
        'docs/ROADMAP.md',
        'docs/PLAN.md',
        'assets/logo.png',
        'design/mark.svg',
        'scripts/release.sh',
      ].map(path => ({ path, name: path.split('/').pop() ?? path }))
      return query ? list.filter(item => item.path.toLowerCase().includes(query)) : list
    }
    case 'context_stats':
      return { system: 178, tools: 3610, user: 1180, assistant: 2360, tool: 18040 }
    case 'engine_catalog':
      return {
        engine: String(args.engine ?? 'native'),
        available: true,
        reason: null,
        commands: [],
        models: [
          { id: 'deepseek-chat', label: 'DeepSeek Chat', provider: 'deepseek', variants: [], defaultVariant: null, isDefault: true },
          { id: 'deepseek-reasoner', label: 'DeepSeek Reasoner', provider: 'deepseek', variants: [], defaultVariant: null, isDefault: false },
          { id: 'claude-sonnet-4', label: 'Claude Sonnet 4', provider: 'anthropic', variants: [], defaultVariant: null, isDefault: false },
          { id: 'gpt-4o', label: 'GPT-4o', provider: 'openai', variants: [], defaultVariant: null, isDefault: false },
          { id: 'gemini-2.5-pro', label: 'Gemini 2.5 Pro', provider: 'google', variants: [], defaultVariant: null, isDefault: false },
          { id: 'kimi-k2', label: 'Kimi K2', provider: 'moonshot', variants: [], defaultVariant: null, isDefault: false },
          { id: 'qwen3-coder', label: 'Qwen3 Coder', provider: 'qwen', variants: [], defaultVariant: null, isDefault: false },
          { id: 'glm-4.6', label: 'GLM-4.6', provider: 'zhipu', variants: [], defaultVariant: null, isDefault: false },
          { id: 'grok-4', label: 'Grok 4', provider: 'xai', variants: [], defaultVariant: null, isDefault: false },
          { id: 'mistral-large', label: 'Mistral Large', provider: 'mistral', variants: [], defaultVariant: null, isDefault: false },
          { id: 'llama-3.3-70b', label: 'Llama 3.3 70B', provider: 'meta', variants: [], defaultVariant: null, isDefault: false },
        ],
      }
    case 'changes_list':
      return {
        isRepo: true,
        root: NATIVE_DIR,
        branch: 'main',
        files: [
          {
            path: `${NATIVE_DIR}/prism.json`,
            rel: 'prism.json',
            status: 'added',
            untracked: true,
            additions: 11,
            deletions: 0,
            binary: false,
          },
          {
            path: `${NATIVE_DIR}/src/api.ts`,
            rel: 'src/api.ts',
            status: 'modified',
            untracked: false,
            additions: 12,
            deletions: 3,
            binary: false,
          },
          {
            path: `${NATIVE_DIR}/docs/old.md`,
            rel: 'docs/old.md',
            status: 'deleted',
            untracked: false,
            additions: 0,
            deletions: 8,
            binary: false,
          },
        ],
        additions: 23,
        deletions: 11,
        truncated: false,
      }
    case 'changes_diff':
      return {
        rel: String(args.path ?? ''),
        truncated: false,
        binary: false,
        patch: `diff --git a/src/api.ts b/src/api.ts
index 1a2b3c4..5d6e7f8 100644
--- a/src/api.ts
+++ b/src/api.ts
@@ -1,6 +1,9 @@
 import { invoke } from '@tauri-apps/api/core'
-import type { SessionSummary } from './types'
+import type { SessionSummary, ChangeReport } from './types'
 
 export const api = {
+  changesList: (directory: string) =>
+    invoke<ChangeReport>('changes_list', { directory }),
   listProjects: () => invoke('list_projects'),
 }
`,
      }
    case 'file_text': {
      const path = String(args.path ?? '')
      const text = path.endsWith('.md')
        ? `# Prism 预览\n\n这是一个 **markdown** 文件，段落、列表、代码块都该渲染出来。\n\n- 第一项\n- 第二项\n\n\`\`\`ts\nconst a: number = 1\nexport function hi(name: string) {\n  return \`hello \${name}\`\n}\n\`\`\`\n\n> 引用一句。\n`
        : path.endsWith('.ts') || path.endsWith('.vue')
          ? `import { computed, ref } from 'vue'\n\n// 这一行是注释\nexport const MAX = 4096\n\nexport function count(files: string[]): number {\n  let total = 0\n  for (const file of files) {\n    if (!file.startsWith('node_modules')) total += 1\n  }\n  return total\n}\n`
          : `{
  "engine": "native",
  "providers": [
    { "id": "deepseek", "baseUrl": "https://api.deepseek.com/v1" }
  ],
  "maxTurns": 12,
  "stream": true,
  "budget": null
}
`
      return { path, size: text.length, truncated: false, binary: false, text }
    }
    case 'read_artifact': {
      const path = String(args.path ?? '')
      if (path.endsWith('.svg')) {
        return {
          kind: 'svg',
          name: 'icon.svg',
          size: 120,
          mime: 'image/svg+xml',
          text: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="12" fill="#6d5ae6"/><path d="M22 14 L42 50 L2 50 Z" fill="none" stroke="#fff" stroke-width="4"/></svg>',
        }
      }
      return {
        kind: 'image',
        name: 'shot.png',
        size: 1024,
        mime: 'image/png',
        base64:
          'iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAFh0lEQVR42u3Pa0oyXQAA4HcvZpJZmBfMjC6GmqQWdpGcpppGMRs/kRCRUCQiIkKRiIiIRCIiRIoQiYgoQiQiok2c+9nDtwj/nmcHz7//okCTwI4MkhtgWwEJFcTjIJYAahJsaUBJgc00WM8AeResZYGUA9E8WN0DkQJYKYLlEljaB4sHIHwIFo7A/DEInYBgGQQqYK4K/Kdg9gz4zoH3AnguwcwVcF+D6RqYqoPJGzBxC8bvgOsejDWAswlGH4DjEdifgK0FrG1geQYjL8D8CobfwNA7MH2AwU9g7ICBLjB8gf5voP8Bfb9A9wf+iYAIiIAIiECPAahJcEeGyQ24rcCECuNxGEtANQm3NKik4GYarmegvAvXslDKwWgeru7BSAGuFOFyCS7tw8UDGD6EC0dw/hiGTmCwDAMVOFeF/lM4ewZ959B7AT2XcOYKuq/hdA1O1eHkDZy4heN30HUPxxrQ2YSjD9DxCO1P0NaC1ja0PMORF2h+hcNvcOgdmj7g4Cc0duBAFxq+YP831P/Avl+o+4MiIAIiIAIi0GsAaRLakVFyA20rKKGieBzFEkhNoi0NKSm0mUbrGSTvorUsknIomkereyhSQCtFtFxCS/to8QCFD9HCEZo/RqETFCyjQAXNVZH/FM2eId858l4gzyWauULuazRdQ1N1NHmDJm7R+B1y3aOxBnI20egDcjwi+xOytZC1jSzPaOQFmV/R8BsaekemDzT4iYwdNNBFhi/U/430P6jvF+n+kAiIgAiIgAj0GsCahHdknNzA2wpOqDgex7EEVpN4S8NKCm+m8XoGy7t4LYulHI7m8eoejhTwShEvl/DSPl48wOFDvHCE549x6AQHyzhQwXNV7D/Fs2fYd469F9hziWeusPsaT9fwVB1P3uCJWzx+h133eKyBnU08+oAdj9j+hG0tbG1jyzMeecHmVzz8hofesekDD35iYwcPdLHhC/d/Y/0P7vvFuj8sAiIgAiIgAr0GiCaRHZkkN8i2QhIqicdJLEHUJNnSiJIim2myniHyLlnLEilHonmyukciBbJSJMslsrRPFg9I+JAsHJH5YxI6IcEyCVTIXJX4T8nsGfGdE+8F8VySmSvivibTNTJVJ5M3ZOKWjN8R1z0ZaxBnk4w+EMcjsT8RW4tY28TyTEZeiPmVDL+RoXdi+iCDn8TYIQNdYvgi/d9E/0P6fonuj4iACIiACIhArwGqSXRHpskNuq3QhErjcRpLUDVJtzSqpOhmmq5nqLxL17JUytFonq7u0UiBrhTpcoku7dPFAxo+pAtHdP6Yhk5osEwDFTpXpf5TOntGfefUe0E9l3Tmirqv6XSNTtXp5A2duKXjd9R1T8ca1Nmkow/U8UjtT9TWotY2tTzTkRdqfqXDb3TonZo+6OAnNXboQJcavmj/N9X/0L5fqvujIiACIiACItBrgGkS25FZcoNtKyyhsnicxRJMTbItjSkptplm6xkm77K1LJNyLJpnq3ssUmArRbZcYkv7bPGAhQ/ZwhGbP2ahExYss0CFzVWZ/5TNnjHfOfNeMM8lm7li7ms2XWNTdTZ5wyZu2fgdc92zsQZzNtnoA3M8MvsTs7WYtc0sz2zkhZlf2fAbG3pnpg82+MmMHTbQZYYv1v/N9D+s75fp/pgIiIAIiIAI9BrgmsR3ZJ7c4NsKT6g8HuexBFeTfEvjSopvpvl6hsu7fC3LpRyP5vnqHo8U+EqRL5f40j5fPODhQ75wxOePeeiEB8s8UOFzVe4/5bNn3HfOvRfcc8lnrrj7mk/X+FSdT97wiVs+fsdd93yswZ1NPvrAHY/c/sRtLW5tc8szH3nh5lc+/MaH3rnpgw9+cmOHD3S54Yv3f3P9D+/75bo/LgIiIAIiIAI9Bf4HvpNkJWgoNHoAAAAASUVORK5CYII=',
      }
    }
    case 'dir_exists':
      return !String(args.path ?? '').includes('old-project')
    case 'write_text':
      return String(args.text ?? '').length
    case 'annotations_get':
      return JSON.parse(JSON.stringify(annotationStore))
    case 'annotation_update': {
      // 星标 / 改名 / 标签都走这里；假后端返回 null 会让调用方把整棵会话头打崩。
      const id = String(args.sessionId ?? '')
      const entry =
        annotationStore.sessions[id] ??
        (annotationStore.sessions[id] = { title: null, starred: false, tags: [] })
      if (args.title !== null && args.title !== undefined) entry.title = String(args.title)
      if (args.starred !== null && args.starred !== undefined) entry.starred = Boolean(args.starred)
      if (args.tags !== null && args.tags !== undefined) entry.tags = args.tags as string[]
      return JSON.parse(JSON.stringify(annotationStore))
    }
    case 'annotation_set_hidden': {
      // 归档里的「关闭空会话 / 恢复」走这里：只改 hidden 标记。
      const ids = (args.sessionIds ?? []) as string[]
      const hidden = Boolean(args.hidden)
      for (const id of ids) {
        const entry =
          annotationStore.sessions[id] ??
          (annotationStore.sessions[id] = { title: null, starred: false, tags: [] })
        entry.hidden = hidden
      }
      return JSON.parse(JSON.stringify(annotationStore))
    }
    case 'browser_state':
      // 预览里没有真 webview：地址栏照常能输入，但页面不会渲染。
      return { url: '', title: '', loading: false, open: false }
    case 'browser_open': {
      const url = String(args.url ?? '')
      return { url, title: url, loading: false, open: false }
    }
    case 'browser_bounds':
    case 'browser_visible':
    case 'browser_history':
    case 'open_url':
      return null
    case 'git_info_batch':
      return {}
    case 'git_branches':
      return []
    case 'runtime_status':
      return {
        engines: [],
        running: 0,
        waiting: 0,
      }
    case 'quota_summary': {
      const idle = {
        state: 'no_subscription',
        accountType: null,
        planType: null,
        primary: null,
        secondary: null,
        credits: null,
        message: null,
      }
      return { fetchedAtMs: Date.now(), codex: { ...idle }, claude: { ...idle } }
    }
    case 'usage_summary':
      return fakeUsage()
    case 'task_list':
      seedTasks()
      return taskStore.map(task => ({ ...task, tail: [...task.tail] }))
    case 'task_spawn': {
      seedTasks()
      const cmd = String(args.cmd ?? '')
      const task = {
        id: `task-fake-${++taskSeq}`,
        cmd,
        cwd: String(args.cwd ?? NATIVE_DIR),
        state: 'running' as const,
        startedAt: Date.now(),
        endedAt: null as number | null,
        exitCode: null as number | null,
        sessionId: (args.sessionId as string | null) ?? null,
        title: (args.title as string | null) ?? null,
        background: args.background !== false,
        origin: (args.origin as 'user' | 'agent' | undefined) ?? 'user',
        tail: [`$ ${cmd}`, 'starting…'],
      }
      taskStore.unshift(task)
      return { ...task, tail: [...task.tail] }
    }
    case 'task_stop': {
      const task = taskStore.find(item => item.id === String(args.id))
      if (task && task.state === 'running') {
        task.state = 'stopped'
        task.endedAt = Date.now()
        task.tail.push('■ 已停止（SIGTERM）')
      }
      return null
    }
    case 'task_remove':
      {
        const index = taskStore.findIndex(item => item.id === String(args.id))
        if (index >= 0) taskStore.splice(index, 1)
      }
      return null
    case 'routines_list':
      return []
    case 'workshop_assets':
      return []
    case 'workshop_mcp':
      return []
    // 工具清单：预览里给一份内置 + 一个外部工具的静态样例，方便看开关样式。
    case 'tool_list':
      // 与 `engines/native/tools.rs` 的 TOOLS 保持同一顺序和数量，
      // 否则预览里的密度和真机差得远，看不出网格几列合适。
      return [
        { name: 'read_file', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '读取文件', tags: [], risk: 'read', network: 'none', sandbox: null },
        { name: 'list_files', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '列出目录', tags: [], risk: 'read', network: 'none', sandbox: null },
        { name: 'grep', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '按内容搜索', tags: [], risk: 'read', network: 'none', sandbox: null },
        { name: 'write_file', origin: 'builtin', permission: 'write', enabled: true, owner: null, description: '写入文件', tags: [], risk: 'write', network: 'none', sandbox: null },
        { name: 'edit_file', origin: 'builtin', permission: 'write', enabled: true, owner: null, description: '局部改文件', tags: [], risk: 'write', network: 'none', sandbox: null },
        { name: 'run_command', origin: 'builtin', permission: 'write', enabled: true, owner: null, description: '执行命令', tags: ['exec'], risk: 'exec', network: 'none', sandbox: null },
        { name: 'task_status', origin: 'builtin', permission: 'write', enabled: false, owner: null, description: '查看后台任务', tags: [], risk: 'write', network: 'none', sandbox: null },
        { name: 'task', origin: 'builtin', permission: 'write', enabled: true, owner: null, description: '委派给子代理', tags: [], risk: 'write', network: 'none', sandbox: null },
        { name: 'fs__read_file', origin: 'plugin', permission: 'write', enabled: false, owner: 'mcp.fs', description: 'Read a file from disk', tags: ['mcp'], risk: 'write', network: 'outbound', sandbox: null },
        { name: 'fs__list_directory', origin: 'plugin', permission: 'write', enabled: false, owner: 'mcp.fs', description: 'List directory contents', tags: ['mcp'], risk: 'write', network: 'outbound', sandbox: null },
        // 安全研究线的样本工具：预览里要能看到 `bin:static` 标签与只读色点。
        { name: 'bin.meta.identify', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '样本格式识别', tags: ['bin:static'], risk: 'read', network: 'none', sandbox: null },
        { name: 'bin.strings.extract', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '字符串提取', tags: ['bin:static'], risk: 'read', network: 'none', sandbox: null },
        { name: 'bin.pack.detect', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '加壳识别', tags: ['bin:static'], risk: 'read', network: 'none', sandbox: null },
        { name: 'bin.xref.search', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '交叉引用检索', tags: ['bin:static'], risk: 'read', network: 'none', sandbox: null },
        { name: 'bin.crypto.scan', origin: 'builtin', permission: 'read-only', enabled: true, owner: null, description: '密码特征扫描', tags: ['bin:static'], risk: 'read', network: 'none', sandbox: null },
        { name: 'prism.dag.node.add', origin: 'builtin', permission: 'write', enabled: true, owner: null, description: '加节点', tags: ['dag:planner'], risk: 'write', network: 'none', sandbox: null },
      ]
    case 'native_tool_toggle':
      return null
    case 'search_stats':
      return { indexedSessions: 0, indexedMessages: 0, updatedAt: Date.now() }
    case 'plugin:event|listen':
      eventId += 1
      return eventId
    case 'plugin:event|unlisten':
      return null
    case 'plugin:webview|internal_toggle_devtools':
      return null
    case 'agents_list':
      return fakeAgents.map(item => JSON.parse(JSON.stringify(item)))
    case 'agent_save': {
      const profile = JSON.parse(JSON.stringify(args.profile)) as AgentProfile
      const index = fakeAgents.findIndex(item => item.id === profile.id)
      if (index >= 0) {
        // 覆盖内置：留 origin 的原始值，只换内容，前端才能显示「覆盖内置」
        fakeAgents[index] = profile
      } else {
        fakeAgents.push(profile)
      }
      return { ...profile }
    }
    case 'agent_delete': {
      const id = String(args.id)
      const index = fakeAgents.findIndex(item => item.id === id && item.origin === 'user' && !item.overridesBuiltin)
      if (index >= 0) fakeAgents.splice(index, 1)
      return null
    }
    case 'dag_list':
      return fakePlans
        .map(plan => ({
          id: plan.id,
          name: plan.name,
          goal: plan.goal,
          project: plan.project,
          nodeCount: plan.nodes.length,
          gateCount: plan.nodes.filter(node => node.gate?.before).length,
          createdAt: plan.createdAt,
          updatedAt: plan.updatedAt,
        }) satisfies DagSummary)
        .sort((a, b) => b.updatedAt - a.updatedAt)
    case 'dag_get': {
      const plan = fakePlans.find(item => item.id === String(args.id))
      return plan ? JSON.parse(JSON.stringify(plan)) : null
    }
    case 'dag_new':
      return fakePlan(`dag-${Date.now()}`)
    case 'dag_save': {
      const plan = JSON.parse(JSON.stringify(args.plan)) as DagPlan
      plan.updatedAt = Date.now()
      const index = fakePlans.findIndex(item => item.id === plan.id)
      if (index >= 0) fakePlans[index] = plan
      else fakePlans.unshift(plan)
      return JSON.parse(JSON.stringify(plan))
    }
    case 'dag_delete': {
      const id = String(args.id)
      fakePlans = fakePlans.filter(item => item.id !== id)
      return null
    }
    case 'dag_validate': {
      const plan = args.plan as DagPlan
      const issues: { level: 'error' | 'warning'; nodeId: string | null; message: string }[] = []
      for (const node of plan.nodes) {
        if (!node.task.trim()) {
          issues.push({ level: 'error', nodeId: node.id, message: '任务描述是空的，子 Agent 没有可做的事' })
        }
        for (const dep of node.dependsOn) {
          if (!plan.nodes.some(item => item.id === dep)) {
            issues.push({ level: 'error', nodeId: node.id, message: `依赖的节点「${dep}」不在图里` })
          }
        }
        if (!node.agent && !plan.defaults.agent) {
          issues.push({ level: 'warning', nodeId: node.id, message: '没有指定 Agent，也没有计划默认值' })
        }
      }
      return issues
    }
    case 'dag_plan':
      return fakePlan(`dag-${Date.now()}`)
    case 'dag_run': {
      const plan = fakePlans.find(item => item.id === String(args.id))
      if (!plan) throw new Error(`没有这张计划：${String(args.id)}`)
      return JSON.parse(JSON.stringify(startRun(plan)))
    }
    case 'dag_run_cancel': {
      if (!fakeRun) return null
      for (const node of fakeRun.nodes) {
        if (node.status === 'running' || node.status === 'waiting') {
          node.status = 'cancelled'
          node.finishedAt = Date.now()
          node.error = '运行被取消'
        } else if (node.status === 'pending') {
          node.status = 'skipped'
          node.error = '运行被取消'
        }
      }
      finishRun()
      return JSON.parse(JSON.stringify(fakeRun))
    }
    case 'dag_run_approve': {
      if (!fakeRun) return null
      const node = fakeRun.nodes.find(item => item.id === String(args.nodeId))
      if (node && node.status === 'waiting') {
        if (args.allowed) {
          node.status = 'running'
          node.attempt += 1
          node.startedAt = Date.now()
        } else {
          node.status = 'cancelled'
          node.finishedAt = Date.now()
          node.error = '人工确认未通过'
          // 下游跟着跳过：拒绝一个节点等于砍掉它那条分支
          for (const other of fakeRun.nodes) {
            if (other.status === 'pending' && other.id !== node.id) {
              other.status = 'skipped'
              other.error = '上游被拒绝'
            }
          }
        }
      }
      emitRun(fakeRun)
      return JSON.parse(JSON.stringify(fakeRun))
    }
    case 'dag_run_latest': {
      if (!fakeRun || fakeRun.planId !== String(args.planId)) return null
      return JSON.parse(JSON.stringify(fakeRun))
    }
    case 'dag_run_get': {
      if (!fakeRun || fakeRun.id !== String(args.runId)) return null
      return JSON.parse(JSON.stringify(fakeRun))
    }
    case 'plugins_list':
      return fakePlugins.map(item => JSON.parse(JSON.stringify(item)))
    case 'dag_artifacts': {
      const dagId = String(args.dagId ?? '')
      const items = fakeArtifacts
        .filter(item => item.dagId === dagId)
        .map(item => JSON.parse(JSON.stringify(item)))
      return { items, bytes: items.reduce((sum, item) => sum + item.size, 0) }
    }
    case 'dag_artifact_get': {
      const id = String(args.id ?? '')
      const artifact = fakeArtifacts.find(item => item.id === id)
      if (!artifact) throw new Error(`没有这个制品：${id}`)
      return {
        artifact: JSON.parse(JSON.stringify(artifact)),
        text: fakeArtifactText.get(id) ?? '',
        truncated: false,
      }
    }
    case 'dag_runners':
      // 预览里假装三类执行器都装了：真实情况由 `dag.executor.*` 插件决定。
      return ['codex', 'dsh', 'native', 'opencode']
    case 'plugin_set_enabled': {
      const target = fakePlugins.find(item => item.id === String(args.id))
      if (target) {
        target.state = args.enabled ? 'active' : 'disabled'
        target.reason = args.enabled ? null : `${target.id}：被配置关闭（下次启动生效）`
      }
      return null
    }
    default:
      console.warn('[fake] 未实现的命令', cmd, args)
      return null
  }
}

;(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
  invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args),
  transformCallback: (callback: unknown) => {
    eventId += 1
    const id = eventId
    ;(window as unknown as Record<string, unknown>)[`_${id}`] = callback
    return id
  },
  unregisterCallback: (id: number) => {
    delete (window as unknown as Record<string, unknown>)[`_${id}`]
  },
  convertFileSrc: (path: string) => path,
}

/** 预览脚本用：模拟 Rust 的 task-state / task-output（agent 起后台任务只走事件）。 */
function emitTaskEvent(name: string, payload: unknown) {
  for (const listener of eventListeners) {
    if (listener.event !== name) continue
    listener.fn({ event: name, id: 0, payload })
  }
}

;(window as unknown as Record<string, unknown>).__emitTaskState = (task: unknown) =>
  emitTaskEvent('task-state', task)

/** 起一条「像 agent 起的」后台任务：只发事件，不走 task_spawn 返回值。 */
;(window as unknown as Record<string, unknown>).__agentTask = (cmd = 'for i in 1 2 3 4 5 6; do echo tick $i; sleep 1; done') => {
  seedTasks()
  const task = {
    id: `task-agent-${++taskSeq}`,
    cmd,
    cwd: NATIVE_DIR,
    state: 'running' as 'running' | 'done',
    startedAt: Date.now(),
    endedAt: null as number | null,
    exitCode: null as number | null,
    sessionId: firstSessionId,
    title: null,
    background: true,
    origin: 'agent' as const,
    tail: [`$ ${cmd}`],
  }
  taskStore.unshift(task)
  emitTaskEvent('task-state', { ...task, tail: [...task.tail] })
  let tick = 0
  const timer = window.setInterval(() => {
    tick += 1
    const line = `tick ${tick}`
    task.tail.push(line)
    emitTaskEvent('task-output', { id: task.id, line, stream: 'stdout' })
    if (tick >= 6) {
      window.clearInterval(timer)
      task.state = 'done'
      task.endedAt = Date.now()
      task.exitCode = 0
      emitTaskEvent('task-state', { ...task, tail: [...task.tail] })
    }
  }, 800)
  return task.id
}

/** 预览脚本用：推一条引擎事件（形状同 Rust 的 engine-event）。 */
;(window as unknown as Record<string, unknown>).__emitEngine = (event: unknown) => {
  for (const listener of eventListeners) {
    if (listener.event !== 'engine-event') continue
    listener.fn({ event: 'engine-event', id: 0, payload: event })
  }
}

console.info('[fake] 场景就绪：', scene)
