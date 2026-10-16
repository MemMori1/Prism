<script setup lang="ts">
import { onMounted, ref, computed } from 'vue'
import { api } from './api'
import type { EngineHealth, ProjectItem, SessionSummary } from './types'
import ProjectsPane from './components/ProjectsPane.vue'
import SessionsPane from './components/SessionsPane.vue'
import SessionView from './components/SessionView.vue'

const health = ref<EngineHealth | null>(null)
const error = ref<string | null>(null)

const projects = ref<ProjectItem[]>([])
const projectsLoading = ref(false)
const selectedProjectId = ref<string | null>(null)

const sessions = ref<SessionSummary[]>([])
const sessionsLoading = ref(false)
const selectedSessionId = ref<string | null>(null)

const selectedProject = computed(
  () => projects.value.find(project => project.id === selectedProjectId.value) ?? null,
)
const selectedSession = computed(
  () => sessions.value.find(session => session.id === selectedSessionId.value) ?? null,
)

const engineStatus = computed(() => {
  if (!health.value) return { label: '连接中…', tone: 'muted' as const }
  if (health.value.ready) return { label: '已就绪', tone: 'ok' as const }
  if (health.value.databasePath) return { label: '只读（未找到 CLI）', tone: 'warn' as const }
  return { label: '未安装', tone: 'error' as const }
})

function reportError(cause: unknown) {
  error.value = cause instanceof Error ? cause.message : String(cause)
  window.setTimeout(() => {
    if (error.value) error.value = null
  }, 6000)
}

async function loadProjects() {
  projectsLoading.value = true
  try {
    projects.value = await api.listProjects()
    if (!projects.value.some(project => project.id === selectedProjectId.value)) {
      selectedProjectId.value = null
      selectedSessionId.value = null
      sessions.value = []
    }
  } catch (cause) {
    reportError(cause)
  } finally {
    projectsLoading.value = false
  }
}

async function selectProject(projectId: string) {
  if (selectedProjectId.value === projectId) return
  selectedProjectId.value = projectId
  selectedSessionId.value = null
  sessions.value = []
  await loadSessions(projectId)
}

async function loadSessions(projectId: string) {
  sessionsLoading.value = true
  try {
    const loaded = await api.listSessions(projectId)
    if (selectedProjectId.value !== projectId) return
    sessions.value = loaded
    if (loaded.length > 0) {
      await selectSession(loaded[0].id)
    }
  } catch (cause) {
    reportError(cause)
  } finally {
    sessionsLoading.value = false
  }
}

function selectSession(sessionId: string) {
  selectedSessionId.value = sessionId
}

async function reload() {
  try {
    health.value = await api.engineHealth()
  } catch (cause) {
    reportError(cause)
  }
  await loadProjects()
  const first = projects.value[0]
  if (!selectedProjectId.value && first) {
    await selectProject(first.id)
  } else if (selectedProjectId.value) {
    await loadSessions(selectedProjectId.value)
  }
}

onMounted(reload)
</script>

<template>
  <div class="app">
    <header class="topbar">
      <div class="brand">
        <svg class="brand-mark" viewBox="0 0 64 64" aria-hidden="true">
          <path d="M22 14 L42 50 L2 50 Z" fill="none" stroke="currentColor" stroke-width="4" stroke-linejoin="round" />
          <path d="M0 33 H21" stroke="currentColor" stroke-width="4" stroke-linecap="round" />
          <path d="M39 27 L62 16" stroke="#8b5cf6" stroke-width="3.6" stroke-linecap="round" />
          <path d="M41 33 H62" stroke="#22d3ee" stroke-width="3.6" stroke-linecap="round" />
          <path d="M39 39 L62 50" stroke="#f472b6" stroke-width="3.6" stroke-linecap="round" />
        </svg>
        <span class="brand-name">Prism</span>
      </div>

      <div v-if="health" class="engine-chip" :class="`tone-${engineStatus.tone}`" :title="health.databasePath ?? ''">
        <span class="dot" />
        <span>OpenCode · {{ engineStatus.label }}</span>
      </div>

      <div class="topbar-spacer" />

      <button class="refresh-btn" :disabled="projectsLoading" @click="reload">刷新</button>
    </header>

    <transition name="fade">
      <div v-if="error" class="error-banner">{{ error }}</div>
    </transition>

    <div class="panes">
      <ProjectsPane
        :projects="projects"
        :selected-id="selectedProjectId"
        :loading="projectsLoading"
        @select="selectProject"
      />
      <SessionsPane
        :sessions="sessions"
        :selected-id="selectedSessionId"
        :loading="sessionsLoading"
        :project="selectedProject"
        @select="selectSession"
      />
      <SessionView
        :session="selectedSession"
        :has-project="!!selectedProject"
        @error="reportError"
      />
    </div>
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: column;
  height: 100%;
}

.topbar {
  display: flex;
  align-items: center;
  gap: 12px;
  height: 46px;
  padding: 0 14px;
  border-bottom: 1px solid var(--border);
  background: var(--panel);
  flex: none;
}

.brand {
  display: flex;
  align-items: center;
  gap: 8px;
}

.brand-mark {
  width: 20px;
  height: 20px;
  color: var(--text);
}

.brand-name {
  font-weight: 650;
  font-size: 14px;
  letter-spacing: 0.2px;
}

.engine-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 12px;
}

.engine-chip .dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--faint);
}

.engine-chip.tone-ok .dot {
  background: var(--green);
}

.engine-chip.tone-warn .dot {
  background: var(--amber);
}

.engine-chip.tone-error .dot {
  background: var(--red);
}

.topbar-spacer {
  flex: 1;
}

.refresh-btn {
  padding: 4px 12px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  transition: all 0.15s ease;
}

.refresh-btn:hover:not(:disabled) {
  color: var(--text);
  border-color: var(--border-strong);
}

.refresh-btn:disabled {
  opacity: 0.5;
  cursor: default;
}

.error-banner {
  position: absolute;
  top: 54px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 20;
  max-width: 70%;
  padding: 8px 16px;
  border-radius: var(--radius);
  background: var(--red-soft);
  color: var(--red);
  border: 1px solid color-mix(in srgb, var(--red) 30%, transparent);
  box-shadow: var(--shadow);
  font-size: 12px;
}

.panes {
  display: flex;
  flex: 1;
  min-height: 0;
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s ease;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
</style>
