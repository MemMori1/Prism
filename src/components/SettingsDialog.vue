<script setup lang="ts">
/**
 * 设置对话框：引擎状态一眼看清（安装 / 版本 / 运行轴）。
 * 挂载即拉取一次；关闭由父级控制（v-if）。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { getVersion } from '@tauri-apps/api/app'
import { api } from '../api'
import type { EngineHealth, RuntimeStatus } from '../types'

const emit = defineEmits<{
  (e: 'close'): void
}>()

const engines = ref<EngineHealth[]>([])
const cliVersion = ref<string | null>(null)
const runtime = ref<RuntimeStatus | null>(null)
const prismVersion = ref<string | null>(null)
const loading = ref(true)
const errorText = ref<string | null>(null)

const opencode = computed(() => engines.value.find(item => item.engineId === 'opencode') ?? null)
const codex = computed(() => engines.value.find(item => item.engineId === 'codex') ?? null)
const claude = computed(() => engines.value.find(item => item.engineId === 'claude') ?? null)

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

<style scoped>
.settings-overlay {
  position: fixed;
  inset: 0;
  z-index: 50;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(10, 8, 20, 0.45);
  backdrop-filter: blur(3px);
}

.settings-panel {
  width: min(560px, calc(100vw - 48px));
  max-height: calc(100vh - 96px);
  display: flex;
  flex-direction: column;
  border-radius: var(--radius);
  border: 1px solid var(--border-strong);
  background: var(--panel);
  box-shadow: var(--shadow);
  overflow: hidden;
}

.settings-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 13px 18px;
  border-bottom: 1px solid var(--border);
}

.settings-title {
  font-size: 14px;
  font-weight: 650;
}

.close-btn {
  width: 26px;
  height: 26px;
  border-radius: var(--radius-sm);
  color: var(--muted);
  font-size: 12px;
  line-height: 1;
  transition: all 0.15s ease;
}

.close-btn:hover {
  color: var(--text);
  background: var(--panel-2);
}

.settings-body {
  padding: 6px 18px 16px;
  overflow-y: auto;
}

.settings-error {
  margin: 10px 0 2px;
  padding: 7px 11px;
  border-radius: var(--radius-sm);
  background: var(--red-soft);
  color: var(--red);
  font-size: 12px;
}

.settings-section {
  margin-top: 14px;
}

.engine-block + .engine-block {
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px dashed var(--border);
}

.engine-name {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  font-weight: 650;
  margin-bottom: 2px;
}

.engine-tag {
  padding: 0 5px;
  border-radius: 4px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--faint);
  font-size: 9.5px;
  font-weight: 600;
  letter-spacing: 0.3px;
}

.section-label {
  color: var(--faint);
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.6px;
  text-transform: uppercase;
  margin-bottom: 6px;
}

.info-row {
  display: flex;
  align-items: baseline;
  gap: 12px;
  padding: 5px 0;
}

.info-key {
  flex: none;
  width: 72px;
  color: var(--muted);
  font-size: 12px;
}

.info-value {
  flex: 1;
  min-width: 0;
  font-size: 12.5px;
  display: flex;
  align-items: center;
  gap: 6px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.info-value.mono {
  font-family: var(--font-mono);
  font-size: 11.5px;
  color: var(--muted);
  display: block;
}

.status-dot {
  flex: none;
  display: inline-block;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--faint);
}

.status-dot.tone-ok {
  background: var(--green);
}

.status-dot.tone-warn {
  background: var(--amber);
}

.status-dot.tone-error {
  background: var(--red);
}

.settings-note {
  margin-top: 16px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
  color: var(--faint);
  font-size: 11px;
  line-height: 1.6;
}

.settings-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 11px 18px;
  border-top: 1px solid var(--border);
  background: var(--panel-2);
}

.prism-version {
  color: var(--faint);
  font-family: var(--font-mono);
  font-size: 11px;
}

.recheck-btn {
  padding: 5px 14px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border-strong);
  background: var(--panel);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  transition: all 0.15s ease;
}

.recheck-btn:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}

.recheck-btn:disabled {
  opacity: 0.55;
  cursor: default;
}
</style>
