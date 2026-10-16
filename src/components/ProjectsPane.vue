<script setup lang="ts">
import type { ProjectItem } from '../types'
import { basename } from '../format'

defineProps<{
  projects: ProjectItem[]
  selectedId: string | null
  loading: boolean
}>()

const emit = defineEmits<{
  (e: 'select', projectId: string): void
}>()
</script>

<template>
  <aside class="pane projects">
    <div class="pane-header">
      <span class="pane-title">项目</span>
      <span v-if="projects.length" class="pane-count">{{ projects.length }}</span>
    </div>

    <div class="pane-body">
      <div v-if="loading && !projects.length" class="pane-empty">加载中…</div>
      <div v-else-if="!projects.length" class="pane-empty">
        未发现任何项目<br />
        <span class="hint">用 OpenCode 跑过会话后会自动出现</span>
      </div>

      <button
        v-for="project in projects"
        :key="project.id"
        class="project-item"
        :class="{ active: project.id === selectedId }"
        :title="project.worktree"
        @click="emit('select', project.id)"
      >
        <span class="project-name">{{ project.name || basename(project.worktree) }}</span>
        <span class="project-path">{{ project.worktree }}</span>
        <span class="project-count">{{ project.sessionCount }}</span>
      </button>
    </div>
  </aside>
</template>

<style scoped>
.pane {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-right: 1px solid var(--border);
  background: var(--panel);
}

.projects {
  width: 216px;
  flex: none;
}

.pane-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 14px 6px;
  flex: none;
}

.pane-title {
  font-size: 11px;
  font-weight: 650;
  text-transform: uppercase;
  letter-spacing: 0.8px;
  color: var(--faint);
}

.pane-count {
  font-size: 11px;
  color: var(--faint);
}

.pane-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 4px 8px 10px;
}

.pane-empty {
  padding: 18px 10px;
  color: var(--faint);
  font-size: 12px;
  line-height: 1.7;
}

.hint {
  font-size: 11px;
}

.project-item {
  display: grid;
  grid-template-columns: 1fr auto;
  grid-template-rows: auto auto;
  gap: 1px 8px;
  width: 100%;
  text-align: left;
  padding: 7px 9px;
  border-radius: var(--radius-sm);
  transition: background 0.12s ease;
}

.project-item:hover {
  background: var(--panel-2);
}

.project-item.active {
  background: var(--accent-soft);
}

.project-name {
  font-weight: 560;
  font-size: 12.5px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.project-item.active .project-name {
  color: var(--accent-text);
}

.project-path {
  grid-column: 1 / 2;
  font-size: 10.5px;
  color: var(--faint);
  font-family: var(--font-mono);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  direction: rtl;
  text-align: left;
}

.project-count {
  grid-row: 1 / 2;
  align-self: center;
  font-size: 10.5px;
  color: var(--faint);
  background: var(--panel-2);
  border: 1px solid var(--border);
  padding: 0 6px;
  border-radius: 999px;
  line-height: 16px;
}

.project-item.active .project-count {
  background: var(--panel);
  color: var(--accent-text);
  border-color: color-mix(in srgb, var(--accent) 30%, transparent);
}
</style>
