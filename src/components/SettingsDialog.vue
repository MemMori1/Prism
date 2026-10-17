<script setup lang="ts">
/**
 * 设置对话框：引擎状态一眼看清（安装 / 版本 / 运行轴）+ 应用更新。
 * 挂载即拉取一次；关闭由父级控制（v-if）。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { getVersion } from '@tauri-apps/api/app'
import { relaunch } from '@tauri-apps/plugin-process'
import { check, type Update } from '@tauri-apps/plugin-updater'
import { api } from '../api'
import type { EngineHealth, RuntimeStatus } from '../types'
import { friendlyUpdateError } from '../updateStatus'
import { readTheme, setThemeMode, THEME_MODES, type ThemeMode } from '../theme'

const emit = defineEmits<{
  (e: 'close'): void
}>()

const engines = ref<EngineHealth[]>([])
const cliVersion = ref<string | null>(null)
const runtime = ref<RuntimeStatus | null>(null)
const prismVersion = ref<string | null>(null)
const loading = ref(true)
const errorText = ref<string | null>(null)

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

const opencode = computed(() => engines.value.find(item => item.engineId === 'opencode') ?? null)
const codex = computed(() => engines.value.find(item => item.engineId === 'codex') ?? null)
const claude = computed(() => engines.value.find(item => item.engineId === 'claude') ?? null)

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

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

onMounted(() => {
  void load()
  void loadPrismVersion()
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
    <div class="settings-panel" role="dialog" aria-label="设置">
      <header class="settings-header">
        <span class="settings-title">设置</span>
        <button class="close-btn" title="关闭（Esc）" @click="emit('close')">✕</button>
      </header>

      <div class="settings-body">
        <div v-if="errorText" class="settings-error">{{ errorText }}</div>

        <section class="settings-section">
          <div class="section-label">引擎</div>

          <div class="engine-block">
            <div class="engine-name">OpenCode</div>
            <div class="info-row">
              <span class="info-key">状态</span>
              <span class="info-value">
                <span class="status-dot" :class="`tone-${statusOf(opencode).tone}`" />
                {{ statusOf(opencode).label }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">版本</span>
              <span class="info-value mono">
                {{ cliVersion ?? (opencode?.cliPath ? '（读取失败）' : '未检测到 CLI') }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">CLI</span>
              <span class="info-value mono" :title="opencode?.cliPath ?? ''">
                {{ opencode?.cliPath ?? '未找到' }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">数据库</span>
              <span class="info-value mono" :title="opencode?.databasePath ?? ''">
                {{ opencode?.databasePath ?? '未找到（先用 OpenCode 跑过会话）' }}
              </span>
            </div>
          </div>

          <div class="engine-block">
            <div class="engine-name">
              Codex
              <span class="engine-tag">浏览 + 运行</span>
            </div>
            <div class="info-row">
              <span class="info-key">状态</span>
              <span class="info-value">
                <span class="status-dot" :class="`tone-${statusOf(codex).tone}`" />
                {{ statusOf(codex).label }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">CLI</span>
              <span class="info-value mono" :title="codex?.cliPath ?? ''">
                {{ codex?.cliPath ?? '未找到' }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">状态库</span>
              <span class="info-value mono" :title="codex?.databasePath ?? ''">
                {{ codex?.databasePath ?? '未找到（先用 Codex 跑过会话）' }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">说明</span>
              <span class="info-value">
                浏览与运行均已接入（发消息走本机 codex app-server，继承你的配置）
              </span>
            </div>
          </div>

          <div class="engine-block">
            <div class="engine-name">
              Claude Code
              <span class="engine-tag">历史浏览</span>
            </div>
            <div class="info-row">
              <span class="info-key">状态</span>
              <span class="info-value">
                <span class="status-dot" :class="`tone-${statusOf(claude).tone}`" />
                {{ statusOf(claude).label }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">CLI</span>
              <span class="info-value mono" :title="claude?.cliPath ?? ''">
                {{ claude?.cliPath ?? '未找到（安装 claude CLI 后可接入运行轴）' }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">档案目录</span>
              <span class="info-value mono" :title="claude?.databasePath ?? ''">
                {{ claude?.databasePath ?? '未找到（先用 Claude Code 跑过会话）' }}
              </span>
            </div>
            <div class="info-row">
              <span class="info-key">说明</span>
              <span class="info-value">
                浏览已接入（会话档案 JSONL）；运行轴待本机安装 claude CLI 后接入
              </span>
            </div>
          </div>
        </section>

        <section class="settings-section">
          <div class="section-label">运行轴（OpenCode）</div>
          <div class="info-row">
            <span class="info-key">状态</span>
            <span class="info-value">
              <span
                class="status-dot"
                :class="runtime?.running ? 'tone-ok' : 'tone-muted'"
              />
              {{ runtime?.running ? '运行中' : '未启动（首次发消息时自动拉起）' }}
            </span>
          </div>
          <div v-if="runtime?.running" class="info-row">
            <span class="info-key">进程</span>
            <span class="info-value mono">
              PID {{ runtime.pid }} · {{ runtime.baseUrl }}
            </span>
          </div>
        </section>

        <section class="settings-section">
          <div class="section-label">外观</div>
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
          <div class="theme-hint">跟随系统时随 macOS 外观自动切换，浅色 / 深色会覆盖系统设置。</div>
        </section>

        <section class="settings-section">
          <div class="section-label">更新</div>
          <div class="update-row">
            <span class="update-text" :class="{ 'is-error': updatePhase === 'error' }">
              {{ updateText }}
            </span>
            <button class="recheck-btn" :disabled="updateBusy" @click="onUpdateAction">
              {{ updateActionLabel }}
            </button>
          </div>
        </section>

        <div class="settings-note">
          引擎数据一律只读，Prism 不写引擎的数据库与状态库；一切都在本机运行。
        </div>
      </div>

      <footer class="settings-footer">
        <span class="prism-version">
          Prism {{ prismVersion ? `v${prismVersion}` : '' }}
        </span>
        <button class="recheck-btn" :disabled="loading" @click="load">
          {{ loading ? '检测中…' : '重新检测' }}
        </button>
      </footer>
    </div>
  </div>
</template>

<style scoped src="../styles/components/SettingsDialog.css"></style>
