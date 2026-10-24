<script setup lang="ts">
/**
 * 设置工作区：左侧分类 + 右侧卡片，参考 Monet 的设置布局。
 * 只展示 Prism 真实具备的能力；没有后端开关的分类用说明代替假控件。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { getVersion } from '@tauri-apps/api/app'
import { relaunch } from '@tauri-apps/plugin-process'
import { check, type Update } from '@tauri-apps/plugin-updater'
import { api } from '../api'
import type {
  AssetItem,
  AssetKind,
  BackendView,
  DshPermissionView,
  EngineHealth,
  McpServer,
  RuntimeStatus,
  ToolView,
} from '../types'
import type { AppView } from '../nav'
import {
  SETTINGS_SECTIONS,
  type SettingsSectionId,
} from '../settingsSections'
import { friendlyUpdateError } from '../updateStatus'
import { readTheme, setThemeMode, THEME_MODES, type ThemeMode } from '../theme'
import ModelProviders from './ModelProviders.vue'
import PhoneLink from './PhoneLink.vue'

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'open-view', view: AppView): void
  (e: 'models-changed'): void
}>()

const activeSection = ref<SettingsSectionId>('system')
const currentSection = computed(
  () => SETTINGS_SECTIONS.find(section => section.id === activeSection.value) ?? SETTINGS_SECTIONS[0],
)
const settingsMark = SETTINGS_SECTIONS[0].icon

const engines = ref<EngineHealth[]>([])
const cliVersion = ref<string | null>(null)
const runtime = ref<RuntimeStatus | null>(null)
const prismVersion = ref<string | null>(null)
const loading = ref(true)
const errorText = ref<string | null>(null)

/**
 * dsh 的权限档位。挂在 dsh 进程的环境变量上，改完要重启进程才生效，
 * 所以保存时会把当前对话打断——界面上要先说清楚。
 */
const dshPermission = ref<DshPermissionView | null>(null)
const dshPermissionSaving = ref<string | null>(null)
const dshPermissionError = ref<string | null>(null)
/** dsh 没装的时候这张卡片没有意义，整块隐藏。 */
const dshReady = computed(() => engines.value.some(item => item.engineId === 'dsh'))
/** 当前选了哪档；`null`（没设过）时交给模板显示"跟随 dsh 默认"。 */
const dshCurrent = computed(() => {
  const view = dshPermission.value
  if (!view || view.mode === null) return null
  return view.options.find(item => item.id === view.mode) ?? null
})

const workshopAssets = ref<AssetItem[]>([])
const workshopMcp = ref<McpServer[]>([])
const workshopError = ref(false)

/** 工具清单：内置的 + 插件运行时注册的（MCP）。 */
const tools = ref<ToolView[]>([])
const toolError = ref<string | null>(null)
/** 正在保存开关的工具名：只锁那一行，别的行照常可点。 */
const toolSaving = ref<string | null>(null)

/** 外部程序后端（upx / r2 / Ghidra）：探测结果只在本次进程内有效。 */
const backends = ref<BackendView[]>([])
const toolchainError = ref<string | null>(null)
/** 正在重探的后端 id（`*` = 全部）：只锁按钮，不锁整页。 */
const probing = ref<string | null>(null)
/** 正在手填路径的后端 id；null = 没有在编辑。 */
const editingBackend = ref<string | null>(null)
const draftPath = ref('')

type UpdatePhase = 'idle' | 'checking' | 'latest' | 'available' | 'downloading' | 'ready' | 'error'
const updatePhase = ref<UpdatePhase>('idle')
const updateHandle = ref<Update | null>(null)
const updateProgress = ref<number | null>(null)
const updateError = ref<string | null>(null)

const theme = ref<ThemeMode>(readTheme())
const themeLabels: Record<ThemeMode, string> = {
  system: '跟随系统',
  light: '浅色',
  dark: '深色',
}

function chooseTheme(mode: ThemeMode) {
  theme.value = mode
  setThemeMode(mode)
}

function selectSection(id: SettingsSectionId) {
  activeSection.value = id
}

const opencode = computed(() => engines.value.find(item => item.engineId === 'opencode') ?? null)

const engineRoles: Record<string, string> = {
  opencode: '运行轴',
  codex: '浏览 + 运行',
  dsh: '浏览 + 运行',
  claude: '历史浏览',
  native: '内置运行轴',
}

const engineCards = computed(() =>
  engines.value.map(item => ({
    ...item,
    role: engineRoles[item.engineId] ?? '本机引擎',
    status: statusOf(item),
  })),
)

const cliVersionText = computed(() => {
  if (cliVersion.value) return cliVersion.value
  return opencode.value?.cliPath ? '（读取失败）' : '未检测到 CLI'
})

const assetKindLabels: Record<AssetKind, string> = {
  skill: '技能',
  command: '命令',
  agent: '子代理',
  memory: '记忆',
  plugin: '插件',
}

const assetCounts = computed(() => {
  const counts: Record<AssetKind, number> = {
    skill: 0,
    command: 0,
    agent: 0,
    memory: 0,
    plugin: 0,
  }
  for (const item of workshopAssets.value) counts[item.kind] += 1
  return counts
})

const assetKindList = computed(() =>
  (Object.keys(assetKindLabels) as AssetKind[]).map(kind => ({
    kind,
    label: assetKindLabels[kind],
    count: assetCounts.value[kind],
  })),
)

const workshopMcpCount = computed(() => workshopMcp.value.length)

/**
 * 工具分组：内置 / 外部。
 *
 * 分组的理由不是排版，是信任：内置工具编译进二进制、默认开；外部工具由
 * 插件运行时注册、默认关，打开等于给一段第三方代码执行权。把它们混在一张
 * 清单里，用户会以为开关的是同一类东西。
 */
const toolGroups = computed(() => [
  {
    id: 'builtin',
    label: '系统内置',
    hint: '随 Prism 编译 · 点击关闭',
    items: tools.value.filter(tool => tool.origin === 'builtin'),
    empty: '没有内置工具——工具插件可能被关掉了。',
  },
  {
    id: 'external',
    label: '后续新增',
    hint: 'MCP / 插件提供 · 点击开启',
    items: tools.value.filter(tool => tool.origin !== 'builtin'),
    empty: '还没有新增工具。在 ~/.prism/mcp.json 里配置后重启 Prism。',
  },
])

const updateText = computed(() => {
  switch (updatePhase.value) {
    case 'idle':
      return '检查是否有新版本'
    case 'checking':
      return '正在检查更新…'
    case 'latest':
      return '已是最新版本'
    case 'available':
      return `发现新版本 v${updateHandle.value?.version ?? ''}`
    case 'downloading':
      return updateProgress.value === null
        ? '正在下载…'
        : `正在下载 ${Math.round(updateProgress.value)}%`
    case 'ready':
      return `v${updateHandle.value?.version ?? ''} 已下载，重启后生效`
    case 'error':
      return updateError.value ?? '检查失败'
  }
})

const updateBusy = computed(
  () => updatePhase.value === 'checking' || updatePhase.value === 'downloading',
)

const updateActionLabel = computed(() => {
  switch (updatePhase.value) {
    case 'checking':
      return '检查中…'
    case 'downloading':
      return '下载中…'
    case 'available':
      return '下载并安装'
    case 'ready':
      return '重启生效'
    case 'latest':
      return '再检查一次'
    case 'error':
      return '重试'
    default:
      return '检查更新'
  }
})

async function load() {
  loading.value = true
  errorText.value = null
  try {
    const [allEngines, version, status] = await Promise.all([
      api.engineHealth(),
      api.cliVersion(),
      api.runtimeStatus(),
    ])
    engines.value = allEngines
    cliVersion.value = version
    runtime.value = status
  } catch (cause) {
    errorText.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    loading.value = false
  }
  // 单独一条，别让它拖垮整页：dsh 没装时这个命令也可能失败。
  void loadDshPermission()
}

async function loadDshPermission() {
  try {
    dshPermission.value = await api.dshPermission()
    dshPermissionError.value = null
  } catch (cause) {
    dshPermissionError.value = cause instanceof Error ? cause.message : String(cause)
  }
}

/**
 * 切换档位。
 *
 * `null` = 恢复 dsh 默认（不设环境变量）。后端会顺手关掉 dsh 进程，
 * 因为权限只在 spawn 时读一次——正在跑的回合会断，所以卡片上写了提示。
 */
async function pickDshPermission(mode: string | null) {
  if (dshPermissionSaving.value !== null) return
  if ((dshPermission.value?.mode ?? null) === mode) return
  dshPermissionSaving.value = mode ?? 'reset'
  dshPermissionError.value = null
  try {
    dshPermission.value = await api.dshSetPermission(mode)
  } catch (cause) {
    dshPermissionError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    dshPermissionSaving.value = null
  }
}

async function loadPrismVersion() {
  try {
    prismVersion.value = await getVersion()
  } catch {
    // 拿不到就退化为不显示。
  }
}

async function loadWorkshop() {
  workshopError.value = false
  const [assetsResult, mcpResult] = await Promise.allSettled([
    api.workshopAssets(),
    api.workshopMcp(),
  ])
  if (assetsResult.status === 'fulfilled') workshopAssets.value = assetsResult.value
  if (mcpResult.status === 'fulfilled') workshopMcp.value = mcpResult.value
  workshopError.value =
    assetsResult.status === 'rejected' || mcpResult.status === 'rejected'
}

async function loadTools() {
  toolError.value = null
  try {
    tools.value = await api.toolList()
  } catch (cause) {
    toolError.value = cause instanceof Error ? cause.message : String(cause)
  }
}

async function loadBackends() {
  toolchainError.value = null
  try {
    backends.value = await api.toolchainList()
  } catch (cause) {
    toolchainError.value = cause instanceof Error ? cause.message : String(cause)
  }
}

/** 重探一个后端（或全部）。探测要起进程读版本，按钮期间只锁这一行。 */
async function probeBackend(id?: string) {
  if (probing.value) return
  probing.value = id ?? '*'
  toolchainError.value = null
  try {
    backends.value = await api.toolchainProbe(id ?? null)
    // 后端从"没装"变成"装好了"，工具清单也会跟着变。
    await loadTools()
  } catch (cause) {
    toolchainError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    probing.value = null
  }
}

function startEdit(backend: BackendView) {
  editingBackend.value = backend.id
  draftPath.value = backend.path ?? ''
}

/** 保存手填路径。空字符串 = 恢复自动探测（等价于点「自动」）。 */
async function savePath(id: string) {
  const path = draftPath.value.trim()
  editingBackend.value = null
  toolchainError.value = null
  try {
    backends.value = await api.toolchainSetPath(id, path || null)
    await loadTools()
  } catch (cause) {
    toolchainError.value = cause instanceof Error ? cause.message : String(cause)
  }
}

async function resetPath(id: string) {
  editingBackend.value = null
  toolchainError.value = null
  try {
    backends.value = await api.toolchainSetPath(id, null)
    await loadTools()
  } catch (cause) {
    toolchainError.value = cause instanceof Error ? cause.message : String(cause)
  }
}

/**
 * 翻一个工具的开关。
 *
 * 写的是"显式记录"，下一轮生效：已开着的会话不受影响，新的一轮才拿到新清单。
 * 保存后重新拉一次清单——后端可能因为插件没起来而少了几项，界面得跟着变。
 */
async function toggleTool(tool: ToolView) {
  if (toolSaving.value) return
  toolSaving.value = tool.name
  toolError.value = null
  try {
    await api.nativeToolToggle(tool.name, !tool.enabled)
    await loadTools()
  } catch (cause) {
    toolError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    toolSaving.value = null
  }
}

/** 风险档的中文说法。未知值原样透出，方便后端加档时前端不必同步改。 */
const RISK_LABEL: Record<string, string> = {
  read: '只读',
  write: '可写',
  exec: '执行命令',
  act: '远端操作',
  attack: '主动探测',
}

/** 网络行为的中文说法；`none` 不展示（大多数本地工具都是它，说了等于没说）。 */
const NET_LABEL: Record<string, string> = {
  outbound: '会出网',
  isolated: '仅在隔离环境内联网',
}

const SANDBOX_LABEL: Record<string, string> = {
  code: '需要代码沙箱',
  malware: '需要恶意样本沙箱',
}

/**
 * tooltip 只补充卡片上没地方铺的信息：来源、权限、网络、沙箱、开关状态。
 *
 * 名字和说明已经直接写在卡片里（截断成 `bin.importexp…` 等于没写），
 * 这里带上它们只是为了让悬停时一次看全，不做为主要阅读路径。
 */
function toolTitle(tool: ToolView) {
  const parts = [tool.name]
  if (tool.description) parts.push(tool.description)
  parts.push(tool.owner ? `来自 ${tool.owner}` : '随 Prism 编译')
  parts.push(tool.permission === 'read-only' ? '只读' : '可写工作目录')
  if (tool.risk) parts.push(RISK_LABEL[tool.risk] ?? tool.risk)
  const net = NET_LABEL[tool.network]
  if (net) parts.push(net)
  const sandbox = tool.sandbox ? SANDBOX_LABEL[tool.sandbox] : null
  if (sandbox) parts.push(sandbox)
  if (tool.tags.length) parts.push(`标签 ${tool.tags.join(' ')}`)
  // 后端没接上时，模型根本看不到这个工具——这里说清是"机器缺件"而不是"你关了它"。
  if (tool.needs) parts.push(tool.needs)
  parts.push(tool.enabled ? '已开启' : '已关闭')
  return parts.join(' · ')
}

/** 后端状态的界面说法；未知值原样透出，后端加状态时前端不必同步改。 */
const BACKEND_STATE: Record<string, { label: string; tone: string }> = {
  ready: { label: '已就绪', tone: 'ok' },
  missing: { label: '未检测到', tone: 'error' },
  'too-old': { label: '版本太低', tone: 'warn' },
  broken: { label: '跑不起来', tone: 'error' },
  unknown: { label: '未探测', tone: 'muted' },
}

function backendStateLabel(backend: BackendView) {
  return BACKEND_STATE[backend.state]?.label ?? backend.state
}

function backendTone(backend: BackendView) {
  return BACKEND_STATE[backend.state]?.tone ?? 'muted'
}

/** 路径是从哪儿来的：用户指定 / 环境变量 / PATH / 常见安装位。 */
const SOURCE_LABEL: Record<string, string> = {
  override: '手动指定',
  env: '环境变量',
  path: 'PATH',
  'known-dir': '常见安装位',
}

/**
 * 一行说明：就绪报版本与路径，缺件报安装指引，坏掉报原因。
 *
 * 三种状态用同一行位置，扫视时不用换行找重点。
 */
function backendHint(backend: BackendView) {
  if (backend.state === 'ready') {
    const version = backend.version ? `v${backend.version}` : '版本未知'
    const source = SOURCE_LABEL[backend.source]
    return `${version}${source ? ` · 来自${source}` : ''}${backend.path ? ` · ${backend.path}` : ''}${backend.note ? ` · ${backend.note}` : ''}`
  }
  if (backend.state === 'too-old') {
    const need = backend.minVersion ? `，至少要 ${backend.minVersion}` : ''
    return `检测到 ${backend.version ?? '未知版本'}${need}。升级：${backend.install}`
  }
  if (backend.state === 'broken') {
    return `${backend.note ?? '没探出具体原因'}。装好依赖后点「重新检测」。`
  }
  return backend.install || '没有登记安装指引。'
}

async function checkForUpdate() {
  updatePhase.value = 'checking'
  updateError.value = null
  try {
    const update = await check()
    if (update) {
      updateHandle.value = update
      updatePhase.value = 'available'
    } else {
      updateHandle.value = null
      updatePhase.value = 'latest'
    }
  } catch (cause) {
    updateError.value = friendlyUpdateError(cause instanceof Error ? cause.message : String(cause))
    updatePhase.value = 'error'
  }
}

async function installUpdate() {
  const update = updateHandle.value
  if (!update) return
  updatePhase.value = 'downloading'
  updateProgress.value = null
  try {
    let total = 0
    let downloaded = 0
    await update.downloadAndInstall(event => {
      if (event.event === 'Started') {
        total = event.data.contentLength ?? 0
        updateProgress.value = total > 0 ? 0 : null
      } else if (event.event === 'Progress') {
        downloaded += event.data.chunkLength
        if (total > 0) updateProgress.value = Math.min(100, (downloaded / total) * 100)
      } else if (event.event === 'Finished') {
        updateProgress.value = 100
      }
    })
    updatePhase.value = 'ready'
  } catch (cause) {
    updateError.value = friendlyUpdateError(cause instanceof Error ? cause.message : String(cause))
    updatePhase.value = 'error'
  }
}

async function onUpdateAction() {
  if (updateBusy.value) return
  if (updatePhase.value === 'ready') {
    try {
      await relaunch()
    } catch (cause) {
      updateError.value = friendlyUpdateError(cause instanceof Error ? cause.message : String(cause))
      updatePhase.value = 'error'
    }
    return
  }
  if (updatePhase.value === 'available') {
    await installUpdate()
    return
  }
  await checkForUpdate()
}

function openView(view: AppView) {
  emit('open-view', view)
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

onMounted(() => {
  void load()
  void loadPrismVersion()
  void loadWorkshop()
  void loadTools()
  void loadBackends()
  window.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
})

function statusOf(item: EngineHealth | null): { label: string; tone: string } {
  if (!item) return { label: '检测中…', tone: 'muted' }
  if (item.ready) return { label: '已就绪', tone: 'ok' }
  if (item.databasePath) return { label: '只读（未找到 CLI）', tone: 'warn' }
  return { label: '未安装', tone: 'error' }
}
</script>

<template>
  <div class="settings-overlay" @click.self="emit('close')">
    <div class="settings-panel" role="dialog" aria-modal="true" aria-label="设置">
      <nav class="settings-nav" aria-label="设置分类">
        <div class="settings-nav-brand">
          <span class="settings-nav-mark" aria-hidden="true">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
              <path v-for="d in settingsMark" :key="d" :d="d" />
            </svg>
          </span>
          <span>设置</span>
        </div>

        <button
          v-for="section in SETTINGS_SECTIONS"
          :key="section.id"
          class="settings-nav-item"
          :class="{ active: activeSection === section.id }"
          :aria-current="activeSection === section.id ? 'page' : undefined"
          :aria-label="section.label"
          :title="section.label"
          @click="selectSection(section.id)"
        >
          <svg class="settings-nav-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path v-for="d in section.icon" :key="d" :d="d" />
          </svg>
          <span class="settings-nav-label">{{ section.label }}</span>
        </button>

        <div class="settings-nav-spacer" />
        <div class="settings-nav-foot">
          <span class="settings-nav-foot-title">本机数据</span>
          <span class="settings-nav-foot-text">引擎数据库与配置只读</span>
        </div>
      </nav>

      <div class="settings-main">
        <header class="settings-hero">
          <div class="settings-hero-copy">
            <div class="settings-eyebrow">设置工作区</div>
            <h2 class="settings-heading">{{ currentSection.title }}</h2>
            <p class="settings-intro">{{ currentSection.description }}</p>
          </div>
          <div class="settings-hero-side">
            <span class="settings-hero-icon" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
                <path v-for="d in currentSection.icon" :key="d" :d="d" />
              </svg>
            </span>
            <button class="close-btn" title="关闭（Esc）" aria-label="关闭设置" @click="emit('close')">✕</button>
          </div>
        </header>

        <div class="settings-content">
          <div v-if="errorText" class="settings-error">{{ errorText }}</div>

          <section v-show="activeSection === 'system'" class="settings-page">
            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">应用更新</div>
                <div class="settings-card-hint">检查并安装 Prism 的新版本。</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">当前版本</div>
                  <div class="settings-row-desc">
                    {{ prismVersion ? `Prism v${prismVersion}` : '正在读取版本…' }}
                  </div>
                </div>
                <button class="settings-action" :disabled="updateBusy" @click="onUpdateAction">
                  {{ updateActionLabel }}
                </button>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">更新状态</div>
                  <div class="settings-row-desc" :class="{ 'is-error': updatePhase === 'error' }">
                    {{ updateText }}
                  </div>
                </div>
                <span class="settings-pill">内置更新源</span>
              </div>
            </div>

            <div class="settings-grid">
              <div class="settings-card">
                <header class="settings-card-head">
                  <div class="settings-card-title">本机运行</div>
                  <div class="settings-card-hint">Prism 的数据都留在本机。</div>
                </header>
                <div class="settings-row">
                  <div class="settings-row-main">
                    <div class="settings-row-title">数据访问</div>
                    <div class="settings-row-desc">引擎数据库与状态库只读，不写入、不删除。</div>
                  </div>
                  <span class="settings-pill">只读</span>
                </div>
                <div class="settings-row">
                  <div class="settings-row-main">
                    <div class="settings-row-title">运行位置</div>
                    <div class="settings-row-desc">会话与文件操作均通过本机 CLI 执行。</div>
                  </div>
                  <span class="settings-pill">本机</span>
                </div>
              </div>

              <div class="settings-card">
                <header class="settings-card-head">
                  <div class="settings-card-title">自动化</div>
                  <div class="settings-card-hint">定时任务在自动化页管理。</div>
                </header>
                <div class="settings-row">
                  <div class="settings-row-main">
                    <div class="settings-row-title">定时唤醒</div>
                    <div class="settings-row-desc">按日程触发会话；开关与日志都在自动化页。</div>
                  </div>
                  <button class="settings-action" @click="openView('automation')">打开自动化</button>
                </div>
              </div>
            </div>
          </section>

          <section v-show="activeSection === 'appearance'" class="settings-page">
            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">主题</div>
                <div class="settings-card-hint">选择 Prism 的界面外观。</div>
              </header>
              <div class="settings-row settings-row-stack">
                <div class="settings-row-main">
                  <div class="settings-row-title">外观模式</div>
                  <div class="settings-row-desc">跟随系统时随 macOS 外观自动切换；浅色 / 深色会覆盖系统设置。</div>
                </div>
                <div class="theme-options" role="group" aria-label="主题">
                  <button
                    v-for="mode in THEME_MODES"
                    :key="mode"
                    class="theme-option"
                    :class="{ active: theme === mode }"
                    :aria-pressed="theme === mode"
                    @click="chooseTheme(mode)"
                  >
                    {{ themeLabels[mode] }}
                  </button>
                </div>
              </div>
            </div>

            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">说明</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">作用范围</div>
                  <div class="settings-row-desc">只影响 Prism 的界面，不改变引擎或系统设置。</div>
                </div>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">选择保存</div>
                  <div class="settings-row-desc">主题选择保存在本机，下次启动继续沿用。</div>
                </div>
              </div>
            </div>
          </section>

          <section v-show="activeSection === 'models'" class="settings-page">
            <ModelProviders @changed="emit('models-changed')" />
          </section>

          <section v-show="activeSection === 'phone'" class="settings-page">
            <PhoneLink />
          </section>

          <section v-show="activeSection === 'engines'" class="settings-page">
            <div class="engine-grid">
              <article
                v-for="item in engineCards"
                :key="item.engineId"
                class="settings-card"
              >
                <header class="settings-card-head">
                  <div class="settings-card-title">
                    {{ item.displayName }}
                    <span class="engine-tag">{{ item.role }}</span>
                  </div>
                </header>
                <div class="settings-row settings-row-tight">
                  <span class="settings-kv-key">状态</span>
                  <span class="settings-kv-val">
                    <span class="status-dot" :class="`tone-${item.status.tone}`" />
                    {{ item.status.label }}
                  </span>
                </div>
                <div v-if="item.engineId === 'opencode'" class="settings-row settings-row-tight">
                  <span class="settings-kv-key">版本</span>
                  <span class="settings-kv-val mono">{{ cliVersionText }}</span>
                </div>
                <div class="settings-row settings-row-tight">
                  <span class="settings-kv-key">CLI</span>
                  <span class="settings-kv-val mono" :title="item.cliPath ?? ''">
                    {{ item.cliPath ?? '未找到' }}
                  </span>
                </div>
                <div class="settings-row settings-row-tight">
                  <span class="settings-kv-key">数据库</span>
                  <span class="settings-kv-val mono" :title="item.databasePath ?? ''">
                    {{ item.databasePath ?? '未找到' }}
                  </span>
                </div>
              </article>
            </div>

            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">运行轴（OpenCode）</div>
                <div class="settings-card-hint">首次发消息时会自动拉起本机 OpenCode serve。</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">状态</div>
                  <div class="settings-row-desc">
                    <span class="status-dot" :class="runtime?.running ? 'tone-ok' : 'tone-muted'" />
                    {{ runtime?.running ? '运行中' : '未启动' }}
                  </div>
                </div>
                <span v-if="runtime?.running" class="settings-row-value mono">
                  PID {{ runtime.pid }} · {{ runtime.baseUrl }}
                </span>
              </div>
            </div>
            <div v-if="dshReady" class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">权限（DeepSeek Harness）</div>
                <div class="settings-card-hint">
                  档位挂在 dsh 进程上，切换会重启它，正在跑的回合会中断。
                </div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">当前档位</div>
                  <div class="settings-row-desc">
                    {{ dshCurrent?.description ?? '未设置，跟随 dsh 默认：只能改工作区，且每次写文件/跑命令都要审批' }}
                  </div>
                </div>
                <span class="settings-row-value">{{ dshCurrent?.label ?? '跟随默认' }}</span>
              </div>
              <div class="perm-grid">
                <button
                  v-for="option in dshPermission?.options ?? []"
                  :key="option.id"
                  type="button"
                  class="perm-chip"
                  :class="{ 'is-active': dshPermission?.mode === option.id, 'is-risky': option.risky }"
                  :disabled="dshPermissionSaving !== null"
                  :aria-pressed="dshPermission?.mode === option.id"
                  @click="pickDshPermission(option.id)"
                >
                  <span class="perm-chip-head">
                    <span class="perm-chip-label">{{ option.label }}</span>
                    <span v-if="option.risky" class="perm-chip-flag">谨慎</span>
                  </span>
                  <span class="perm-chip-desc">{{ option.description }}</span>
                </button>
                <button
                  type="button"
                  class="perm-chip"
                  :class="{ 'is-active': !dshPermission?.mode }"
                  :disabled="dshPermissionSaving !== null"
                  :aria-pressed="!dshPermission?.mode"
                  @click="pickDshPermission(null)"
                >
                  <span class="perm-chip-head">
                    <span class="perm-chip-label">跟随 dsh 默认</span>
                  </span>
                  <span class="perm-chip-desc">不设环境变量，用 dsh 自己配的档位</span>
                </button>
              </div>
              <p v-if="dshPermissionError" class="settings-error">{{ dshPermissionError }}</p>
            </div>
          </section>

          <section v-show="activeSection === 'channels'" class="settings-page">
            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">本机接入</div>
                <div class="settings-card-hint">Prism 直接调用本机已安装的引擎 CLI。</div>
              </header>
              <div v-for="item in engineCards" :key="item.engineId" class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">{{ item.displayName }}</div>
                  <div class="settings-row-desc">{{ item.role }} · {{ item.status.label }}</div>
                </div>
                <span class="settings-row-value mono" :title="item.cliPath ?? ''">
                  {{ item.cliPath ?? '未找到 CLI' }}
                </span>
              </div>
            </div>

            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">密钥与请求</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">API Key</div>
                  <div class="settings-row-desc">Prism 不保存 API Key；登录与凭据由各引擎自己的配置管理。</div>
                </div>
                <span class="settings-pill">不保存</span>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">请求代理</div>
                  <div class="settings-row-desc">Prism 不代理模型请求，消息直接交给本机引擎处理。</div>
                </div>
                <span class="settings-pill">不代理</span>
              </div>
            </div>
          </section>

          <section v-show="activeSection === 'intelligence'" class="settings-page">
            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">会话加工</div>
                <div class="settings-card-hint">Prism 不额外改写会话内容。</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">提示词注入</div>
                  <div class="settings-row-desc">不修改你输入的内容，也不追加系统提示词。</div>
                </div>
                <span class="settings-pill">无</span>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">自动标题与摘要</div>
                  <div class="settings-row-desc">由引擎自己决定；Prism 不额外调用模型生成。</div>
                </div>
                <span class="settings-pill">引擎决定</span>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">翻译</div>
                  <div class="settings-row-desc">当前未启用自动翻译。</div>
                </div>
                <span class="settings-pill">未启用</span>
              </div>
            </div>

            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">原则</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">不调用额外模型</div>
                  <div class="settings-row-desc">Prism 只负责展示与调度，模型调用由你选择的引擎完成。</div>
                </div>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">本地优先</div>
                  <div class="settings-row-desc">会话内容不上传到 Prism 的服务器。</div>
                </div>
              </div>
            </div>
          </section>

          <section v-show="activeSection === 'extensions'" class="settings-page">
            <div v-if="workshopError" class="settings-error">工坊数据读取失败，资产与 MCP 数量可能不完整。</div>

            <div class="settings-grid">
              <div class="settings-card">
                <header class="settings-card-head">
                  <div class="settings-card-title">工坊资产</div>
                  <div class="settings-card-hint">技能 / 命令 / 子代理 / 记忆 / 插件，只读浏览。</div>
                </header>
                <div class="extension-stats">
                  <div v-for="stat in assetKindList" :key="stat.kind" class="extension-stat">
                    <div class="extension-stat-value">{{ stat.count }}</div>
                    <div class="extension-stat-label">{{ stat.label }}</div>
                  </div>
                </div>
                <div class="settings-row">
                  <div class="settings-row-main">
                    <div class="settings-row-title">资产来源</div>
                    <div class="settings-row-desc">来自三引擎的用户级、项目级与内置目录。</div>
                  </div>
                  <button class="settings-action" @click="openView('workshop')">打开工坊</button>
                </div>
              </div>

              <div class="settings-card">
                <header class="settings-card-head">
                  <div class="settings-card-title">MCP 服务</div>
                  <div class="settings-card-hint">从引擎配置中解析，不修改原文件。</div>
                </header>
                <div class="settings-row">
                  <div class="settings-row-main">
                    <div class="settings-row-title">已发现</div>
                    <div class="settings-row-desc">用户级与项目级 MCP 定义。</div>
                  </div>
                  <span class="settings-row-value">{{ workshopMcpCount }} 个</span>
                </div>
                <div class="settings-row">
                  <div class="settings-row-main">
                    <div class="settings-row-title">管理入口</div>
                    <div class="settings-row-desc">在工坊中查看命令、环境变量键名与来源文件。</div>
                  </div>
                  <button class="settings-action" @click="openView('workshop')">查看 MCP</button>
                </div>
              </div>
            </div>

            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">工具</div>
                <div class="settings-card-hint">关闭后不进模型清单，下一轮生效。</div>
              </header>

              <div v-if="toolError" class="settings-error">{{ toolError }}</div>

              <div class="tool-sheet">
                <div v-for="group in toolGroups" :key="group.id" class="tool-group">
                  <div class="tool-group-head">
                    <span class="tool-group-title">{{ group.label }}</span>
                    <span class="tool-group-count">{{ group.items.length }}</span>
                    <span class="tool-group-hint">{{ group.hint }}</span>
                  </div>

                  <div v-if="!group.items.length" class="tool-empty">{{ group.empty }}</div>

                  <div v-else class="tool-grid">
                    <button
                      v-for="tool in group.items"
                      :key="tool.name"
                      type="button"
                      class="tool-chip"
                      :class="{ 'is-off': !tool.enabled }"
                      role="switch"
                      :aria-checked="tool.enabled"
                      :aria-label="`${tool.name}：${tool.enabled ? '已开启，点击关闭' : '已关闭，点击开启'}`"
                      :disabled="toolSaving !== null"
                      :title="toolTitle(tool)"
                      @click="toggleTool(tool)"
                    >
                      <span class="tool-chip-top">
                        <span class="tool-chip-name">{{ tool.name }}</span>
                        <span class="tool-chip-dot" :class="`risk-${tool.risk || 'read'}`" />
                      </span>
                      <span class="tool-chip-blurb">{{ tool.description || '没有写说明' }}</span>
                      <span v-if="tool.needs" class="tool-chip-need">{{ tool.needs }}</span>
                      <span v-if="tool.tags.length" class="tool-chip-tag">{{ tool.tags.join(' · ') }}</span>
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </section>

          <section v-show="activeSection === 'toolchain'" class="settings-page">
            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">外部程序</div>
                <div class="settings-card-hint">
                  探测结果只在本次运行内有效；换了版本点「重新检测」。
                </div>
              </header>

              <div v-if="toolchainError" class="settings-error">{{ toolchainError }}</div>

              <div v-if="!backends.length" class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">正在检测…</div>
                  <div class="settings-row-desc">读一次版本号，通常不到一秒。</div>
                </div>
              </div>

              <div v-for="backend in backends" :key="backend.id" class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">
                    {{ backend.name }}
                    <span class="settings-pill" :class="`tone-${backendTone(backend)}`">
                      {{ backendStateLabel(backend) }}
                    </span>
                  </div>
                  <div class="settings-row-desc">{{ backendHint(backend) }}</div>
                  <div v-if="backend.tools.length" class="toolchain-tools">
                    影响：{{ backend.tools.join('、') }}
                  </div>
                  <div v-if="editingBackend === backend.id" class="toolchain-edit">
                    <input
                      v-model="draftPath"
                      class="toolchain-input"
                      :placeholder="backend.install || '可执行文件或安装目录的绝对路径'"
                      spellcheck="false"
                      @keydown.enter="savePath(backend.id)"
                      @keydown.esc="editingBackend = null"
                    />
                    <button class="settings-action" @click="savePath(backend.id)">保存</button>
                    <button class="settings-action" @click="editingBackend = null">取消</button>
                  </div>
                </div>
                <div class="toolchain-actions">
                  <button
                    class="settings-action"
                    :disabled="probing !== null"
                    @click="probeBackend(backend.id)"
                  >
                    {{ probing === backend.id ? '检测中…' : '重新检测' }}
                  </button>
                  <button class="settings-action" @click="startEdit(backend)">
                    {{ backend.overridden ? '改路径' : '指定路径' }}
                  </button>
                  <button
                    v-if="backend.overridden"
                    class="settings-action"
                    @click="resetPath(backend.id)"
                  >
                    自动
                  </button>
                </div>
              </div>
            </div>

            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">为什么只探测</div>
                <div class="settings-card-hint">Prism 不替你装、不替你下载。</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">安装包只有几十 MB</div>
                  <div class="settings-row-desc">
                    Ghidra 自带 JDK 要 400 MB 以上。装到哪、用哪个版本，由你决定；
                    Prism 只负责找到它、按版本约束调用它。
                  </div>
                </div>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">没接上就不给模型看</div>
                  <div class="settings-row-desc">
                    缺后端时对应的工具不进模型清单，模型不会照着记忆编命令；
                    硬调会报「需要 X（未检测到）」并带安装指引。
                  </div>
                </div>
              </div>
            </div>
          </section>

          <section v-show="activeSection === 'permissions'" class="settings-page">
            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">数据边界</div>
                <div class="settings-card-hint">Prism 只读取引擎已经写在本机的数据。</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">引擎数据库</div>
                  <div class="settings-row-desc">会话、项目与用量数据只读，不写入、不删除。</div>
                </div>
                <span class="settings-pill">只读</span>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">引擎配置</div>
                  <div class="settings-row-desc">CLI 配置、MCP 定义与资产文件只读解析。</div>
                </div>
                <span class="settings-pill">只读</span>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">命令执行</div>
                  <div class="settings-row-desc">由引擎按会话内的权限请求处理，Prism 不绕过确认。</div>
                </div>
                <span class="settings-pill">引擎管理</span>
              </div>
            </div>

            <div class="settings-card">
              <header class="settings-card-head">
                <div class="settings-card-title">系统权限</div>
              </header>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">屏幕录制与辅助功能</div>
                  <div class="settings-row-desc">Prism 不主动申请这类权限；如引擎或 macOS 在操作时提示，可按需授权。</div>
                </div>
              </div>
              <div class="settings-row">
                <div class="settings-row-main">
                  <div class="settings-row-title">本机数据</div>
                  <div class="settings-row-desc">会话内容与文件不离开本机，除非引擎自己发起了网络请求。</div>
                </div>
              </div>
            </div>
          </section>
        </div>

        <footer class="settings-footer">
          <span class="prism-version">
            Prism {{ prismVersion ? `v${prismVersion}` : '' }}
          </span>
          <button class="settings-action" :disabled="loading" @click="load">
            {{ loading ? '检测中…' : '重新检测' }}
          </button>
        </footer>
      </div>
    </div>
  </div>
</template>

<style scoped src="../styles/components/SettingsDialog.css"></style>
