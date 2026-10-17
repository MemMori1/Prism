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
import type { AssetItem, AssetKind, EngineHealth, McpServer, RuntimeStatus } from '../types'
import type { AppView } from '../nav'
import {
  SETTINGS_SECTIONS,
  type SettingsSectionId,
} from '../settingsSections'
import { friendlyUpdateError } from '../updateStatus'
import { readTheme, setThemeMode, THEME_MODES, type ThemeMode } from '../theme'

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'open-view', view: AppView): void
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

const workshopAssets = ref<AssetItem[]>([])
const workshopMcp = ref<McpServer[]>([])
const workshopError = ref(false)

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
  claude: '历史浏览',
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
