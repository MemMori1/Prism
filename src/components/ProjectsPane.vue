<script setup lang="ts">
import { computed, watch } from 'vue'
import type { ProjectItem } from '../types'
import { basename } from '../format'
import { ensureGitInfo, gitInfoOf } from '../gitInfo'

const props = defineProps<{
  projects: ProjectItem[]
  selectedId: string | null
  loading: boolean
}>()

const emit = defineEmits<{
  (e: 'select', projectId: string): void
}>()

/** 项目目录就绪即补查 git 信息（分组与分支标注用）。 */
watch(
  () => props.projects.map(project => project.worktree).join('\n'),
  () => {
    void ensureGitInfo(props.projects.map(project => project.worktree))
  },
  { immediate: true },
)

/** 渲染行：同仓（worktree ≥2）收成一组，其余平铺。 */
type Row =
  | { kind: 'group'; key: string; label: string; root: string; count: number }
  | { kind: 'project'; key: string; project: ProjectItem; inGroup: boolean }

const rows = computed<Row[]>(() => {
  const groupsOf = new Map<string, ProjectItem[]>()
  for (const project of props.projects) {
    const root = gitInfoOf(project.worktree)?.repoRoot
    if (!root) continue
    const list = groupsOf.get(root) ?? []
    list.push(project)
    groupsOf.set(root, list)
  }
  const result: Row[] = []
  const emitted = new Set<string>()
  for (const project of props.projects) {
    const root = gitInfoOf(project.worktree)?.repoRoot
    const group = root ? groupsOf.get(root) : undefined
    if (root && group && group.length >= 2) {
      if (!emitted.has(root)) {
        emitted.add(root)
        result.push({
          kind: 'group',
          key: `g:${root}`,
          label: basename(root),
          root,
          count: group.length,
        })
      }
      result.push({ kind: 'project', key: project.id, project, inGroup: true })
    } else {
      result.push({ kind: 'project', key: project.id, project, inGroup: false })
    }
  }
  return result
})

function branchOf(project: ProjectItem): string | null {
  return gitInfoOf(project.worktree)?.branch ?? null
}

function isWorktree(project: ProjectItem): boolean {
  return gitInfoOf(project.worktree)?.isWorktree ?? false
}

const ENGINE_LABELS: Record<string, string> = {
  opencode: 'OC',
  codex: 'CX',
  claude: 'CC',
}

function engineLabel(engine: string): string {
  return ENGINE_LABELS[engine] ?? engine.slice(0, 2).toUpperCase()
}
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
        <span class="hint">用 OpenCode、Codex 或 Claude Code 跑过会话后会自动出现</span>
      </div>

      <template v-for="row in rows" :key="row.key">
        <div v-if="row.kind === 'group'" class="group-head" :title="row.root">
          <span class="group-name">⑂ {{ row.label }}</span>
          <span class="group-count">{{ row.count }} 个工作区</span>
        </div>
        <button
          v-else
          class="project-item"
          :class="{ active: row.project.id === selectedId, 'in-group': row.inGroup }"
          :title="row.project.worktree"
          @click="emit('select', row.project.id)"
        >
          <span class="project-name">{{ row.project.name || basename(row.project.worktree) }}</span>
          <span
            v-if="branchOf(row.project)"
            class="branch-chip"
            :title="`当前分支：${branchOf(row.project)}`"
          >
            <span v-if="isWorktree(row.project)" class="wt-mark">⑂</span>{{ branchOf(row.project) }}
          </span>
          <span class="engine-badge" :class="`engine-${row.project.engine}`">
            {{ engineLabel(row.project.engine) }}
          </span>
          <span class="project-count">{{ row.project.sessionCount }}</span>
          <span class="project-path">{{ row.project.worktree }}</span>
        </button>
      </template>
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
  grid-template-columns: 1fr auto auto auto;
  grid-template-rows: auto auto;
  gap: 1px 8px;
  width: 100%;
  text-align: left;
  padding: 7px 9px;
  border-radius: var(--radius-sm);
  transition: background 0.12s ease;
}

.group-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 9px 9px 3px;
  color: var(--faint);
  font-size: 10.5px;
}

.group-name {
  font-weight: 650;
  letter-spacing: 0.2px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.group-count {
  flex: none;
}

.project-item.in-group {
  margin-left: 7px;
  padding-left: 8px;
  border-left: 2px solid var(--border);
}

.branch-chip {
  align-self: center;
  max-width: 96px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  padding: 0 5px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 9.5px;
  font-family: var(--font-mono);
  line-height: 15px;
}

.wt-mark {
  color: var(--accent-text);
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

.engine-badge {
  grid-row: 1 / 2;
  align-self: center;
  padding: 0 4px;
  border-radius: 4px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--faint);
  font-size: 9.5px;
  font-weight: 650;
  letter-spacing: 0.3px;
}

.engine-badge.engine-codex {
  color: var(--amber);
  border-color: color-mix(in srgb, var(--amber) 35%, var(--border));
}

.engine-badge.engine-claude {
  color: var(--green);
  border-color: color-mix(in srgb, var(--green) 35%, var(--border));
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
