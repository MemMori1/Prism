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

<style scoped src="../styles/components/ProjectsPane.css"></style>
