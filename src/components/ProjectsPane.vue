<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { ProjectItem } from '../types'
import { basename } from '../format'
import { ensureGitInfo, gitInfoOf } from '../gitInfo'
import { pinnedSet, togglePinned } from '../projectPins'

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

/* ---------- 筛选 ---------- */

const filterText = ref('')
const term = computed(() => filterText.value.trim().toLowerCase())

function matchProject(project: ProjectItem): boolean {
  const needle = term.value
  if (!needle) return true
  const name = project.name || basename(project.worktree)
  return (
    name.toLowerCase().includes(needle) ||
    project.worktree.toLowerCase().includes(needle) ||
    project.engine.toLowerCase().includes(needle) ||
    (branchOf(project) ?? '').toLowerCase().includes(needle)
  )
}

const matches = computed(() => props.projects.filter(matchProject))

/* ---------- 行装配：置顶段 + 同仓分组 ---------- */

type Row =
  | { kind: 'section'; key: string; label: string; count: number; icon: 'pin' | null }
  | { kind: 'group'; key: string; label: string; root: string; count: number }
  | { kind: 'project'; key: string; project: ProjectItem; inGroup: boolean; pinned: boolean }

/** 同仓（worktree ≥2）收成一组；置顶项单独成段，不再参与分组。 */
const rows = computed<Row[]>(() => {
  const pinnedProjects: ProjectItem[] = []
  const rest: ProjectItem[] = []
  for (const project of matches.value) {
    if (pinnedSet.value.has(project.worktree)) pinnedProjects.push(project)
    else rest.push(project)
  }
  // 置顶段按记忆顺序排，避免每次扫描抖动。
  const order = [...pinnedSet.value]
  pinnedProjects.sort((a, b) => order.indexOf(a.worktree) - order.indexOf(b.worktree))

  const result: Row[] = []
  if (pinnedProjects.length) {
    result.push({
      kind: 'section',
      key: 's:pinned',
      label: '置顶',
      count: pinnedProjects.length,
      icon: 'pin',
    })
    for (const project of pinnedProjects) {
      result.push({ kind: 'project', key: project.id, project, inGroup: false, pinned: true })
    }
  }

  // 筛选时直接平铺，省掉空分组头。
  if (term.value) {
    for (const project of rest) {
      result.push({ kind: 'project', key: project.id, project, inGroup: false, pinned: false })
    }
    return result
  }

  if (pinnedProjects.length && rest.length) {
    result.push({ kind: 'section', key: 's:all', label: '全部项目', count: rest.length, icon: null })
  }

  const groupsOf = new Map<string, ProjectItem[]>()
  for (const project of rest) {
    const root = gitInfoOf(project.worktree)?.repoRoot
    if (!root) continue
    const list = groupsOf.get(root) ?? []
    list.push(project)
    groupsOf.set(root, list)
  }
  const emitted = new Set<string>()
  for (const project of rest) {
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
      result.push({ kind: 'project', key: project.id, project, inGroup: true, pinned: false })
    } else {
      result.push({ kind: 'project', key: project.id, project, inGroup: false, pinned: false })
    }
  }
  return result
})

const countLabel = computed(() =>
  term.value ? `${matches.value.length}/${props.projects.length}` : `${props.projects.length}`,
)

function select(project: ProjectItem): void {
  emit('select', project.id)
}

function togglePin(project: ProjectItem): void {
  togglePinned(project.worktree)
}

/* ---------- 右键菜单（替代浏览器自带菜单） ---------- */

interface MenuState {
  x: number
  y: number
  project: ProjectItem
}

const MENU_WIDTH = 172
const MENU_HEIGHT = 82

const menu = ref<MenuState | null>(null)
const menuEl = ref<HTMLElement | null>(null)

function openMenu(event: MouseEvent, project: ProjectItem): void {
  event.preventDefault()
  // 贴边时向内翻转，避免菜单被窗口裁掉。
  const x = Math.max(8, Math.min(event.clientX, window.innerWidth - MENU_WIDTH - 8))
  const y = Math.max(8, Math.min(event.clientY, window.innerHeight - MENU_HEIGHT - 8))
  menu.value = { x, y, project }
}

function closeMenu(): void {
  menu.value = null
}

function onMenuPin(): void {
  const project = menu.value?.project
  if (project) togglePinned(project.worktree)
  closeMenu()
}

async function onMenuCopyPath(): Promise<void> {
  const project = menu.value?.project
  closeMenu()
  if (!project) return
  try {
    await navigator.clipboard.writeText(project.worktree)
  } catch {
    // 剪贴板 API 不可用时退回选区复制。
    const area = document.createElement('textarea')
    area.value = project.worktree
    area.style.position = 'fixed'
    area.style.opacity = '0'
    document.body.append(area)
    area.select()
    document.execCommand('copy')
    area.remove()
  }
}

function onWindowPointerDown(event: PointerEvent): void {
  if (!menu.value) return
  if (menuEl.value?.contains(event.target as Node)) return
  closeMenu()
}

function onWindowKeyDown(event: KeyboardEvent): void {
  if (event.key === 'Escape') closeMenu()
}

onMounted(() => {
  window.addEventListener('pointerdown', onWindowPointerDown, true)
  window.addEventListener('keydown', onWindowKeyDown)
  window.addEventListener('resize', closeMenu)
  window.addEventListener('blur', closeMenu)
  window.addEventListener('scroll', closeMenu, true)
})

onBeforeUnmount(() => {
  window.removeEventListener('pointerdown', onWindowPointerDown, true)
  window.removeEventListener('keydown', onWindowKeyDown)
  window.removeEventListener('resize', closeMenu)
  window.removeEventListener('blur', closeMenu)
  window.removeEventListener('scroll', closeMenu, true)
})
</script>

<template>
  <aside class="pane projects">
    <div class="pane-header">
      <div class="pane-headline">
        <span class="pane-title">项目</span>
        <span v-if="projects.length" class="pane-count">{{ countLabel }}</span>
      </div>

      <label class="pane-filter">
        <svg class="filter-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
          <circle cx="10.8" cy="10.8" r="6.8" />
          <path d="m20 20-4.4-4.4" />
        </svg>
        <input
          v-model="filterText"
          class="filter-input"
          type="text"
          placeholder="筛选项目 / 路径 / 分支"
          spellcheck="false"
          autocomplete="off"
          aria-label="筛选项目"
          @keydown.esc="filterText = ''"
        />
        <button v-if="filterText" class="filter-clear" title="清空筛选" @click="filterText = ''">✕</button>
      </label>
    </div>

    <div class="pane-body">
      <div v-if="loading && !projects.length" class="pane-empty">加载中…</div>
      <div v-else-if="!projects.length" class="pane-empty">
        未发现任何项目<br />
        <span class="hint">用 OpenCode、Codex 或 Claude Code 跑过会话后会自动出现</span>
      </div>
      <div v-else-if="!rows.length" class="pane-empty">
        没有匹配「{{ filterText.trim() }}」的项目<br />
        <span class="hint">试试项目名、路径、分支或引擎</span>
      </div>

      <template v-for="row in rows" :key="row.key">
        <div v-if="row.kind === 'section'" class="section-head">
          <span class="section-label">
            <svg
              v-if="row.icon === 'pin'"
              class="section-icon"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M9.5 3.5h5" />
              <path d="M10.5 3.5v4.5L8 10.5v1.5h8v-1.5l-2.5-2.5V3.5" />
              <path d="M12 12v8" />
            </svg>
            {{ row.label }}
          </span>
          <span class="section-count">{{ row.count }}</span>
        </div>

        <div v-else-if="row.kind === 'group'" class="group-head" :title="row.root">
          <span class="group-name">⑂ {{ row.label }}</span>
          <span class="group-count">{{ row.count }} 个工作区</span>
        </div>

        <div
          v-else
          class="project-item"
          :class="{
            active: row.project.id === selectedId,
            'in-group': row.inGroup,
            'has-branch': !!branchOf(row.project),
            pinned: row.pinned,
          }"
          role="button"
          tabindex="0"
          :title="row.project.worktree"
          @click="select(row.project)"
          @keydown.enter.prevent="select(row.project)"
          @keydown.space.prevent="select(row.project)"
          @contextmenu="openMenu($event, row.project)"
        >
          <span class="project-name">
            <svg
              v-if="row.pinned"
              class="pin-mark"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2.2"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M9.5 3.5h5" />
              <path d="M10.5 3.5v4.5L8 10.5v1.5h8v-1.5l-2.5-2.5V3.5" />
              <path d="M12 12v8" />
            </svg>
            <span class="project-name-text">{{ row.project.name || basename(row.project.worktree) }}</span>
          </span>

          <span class="engine-badge" :class="`engine-${row.project.engine}`">
            {{ engineLabel(row.project.engine) }}
          </span>
          <span class="project-count">{{ row.project.sessionCount }}</span>

          <button
            class="project-pin"
            :class="{ 'is-pinned': row.pinned }"
            :title="row.pinned ? '取消置顶' : '置顶项目'"
            :aria-label="row.pinned ? '取消置顶' : '置顶项目'"
            :aria-pressed="row.pinned"
            tabindex="-1"
            @click.stop="togglePin(row.project)"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M9.5 3.5h5" />
              <path d="M10.5 3.5v4.5L8 10.5v1.5h8v-1.5l-2.5-2.5V3.5" />
              <path d="M12 12v8" />
            </svg>
          </button>

          <span v-if="branchOf(row.project)" class="branch-chip" :title="`当前分支：${branchOf(row.project)}`">
            <span v-if="isWorktree(row.project)" class="wt-mark">⑂</span>{{ branchOf(row.project) }}
          </span>
          <span class="project-path"><bdi>{{ row.project.worktree }}</bdi></span>
        </div>
      </template>
    </div>

    <Teleport to="body">
      <div
        v-if="menu"
        ref="menuEl"
        class="project-menu"
        role="menu"
        :style="{ left: `${menu.x}px`, top: `${menu.y}px`, minWidth: `${MENU_WIDTH}px` }"
        @contextmenu.prevent
      >
        <div class="menu-path" :title="menu.project.worktree">
          {{ basename(menu.project.worktree) }}
        </div>
        <button class="menu-item" role="menuitem" @click="onMenuPin">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M9.5 3.5h5" />
            <path d="M10.5 3.5v4.5L8 10.5v1.5h8v-1.5l-2.5-2.5V3.5" />
            <path d="M12 12v8" />
          </svg>
          {{ pinnedSet.has(menu.project.worktree) ? '取消置顶' : '置顶项目' }}
        </button>
        <button class="menu-item" role="menuitem" @click="onMenuCopyPath">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="9" y="9" width="11" height="11" rx="2" />
            <path d="M15 6.5V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h.5" />
          </svg>
          复制路径
        </button>
      </div>
    </Teleport>
  </aside>
</template>

<style scoped src="../styles/components/ProjectsPane.css"></style>
