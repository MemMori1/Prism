<script setup lang="ts">
/**
 * 文件抽屉：左边是这个工作目录的文件，右边看内容 / 改动，能直接改。
 *
 * 两种列表口径：
 * - 变更：相对 HEAD 改了什么（git status / diff，只读；不解析各家工具调用——
 *   一次 shell 命令改到的文件，工具卡里未必看得到）；
 * - 全部：项目里所有文件（git 索引优先，尊重 .gitignore）。
 *
 * 右边三种看法：
 * - 变更：unified diff（只对有改动的文件开放）；
 * - 预览：markdown 渲染、图片直接显示、其余按语言上色 + 行号；
 * - 编辑：纯文本编辑，⌘S 保存（只允许会话目录 / 仓库根内的已存在文件）。
 * 「打开方式」还能交给系统：默认应用 / 在访达中显示 / 复制路径。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import {
  fileKindOf,
  filterProjectFiles,
  joinPath,
  numberedLines,
  splitPath,
  statusLabel,
  statusMark,
} from '../changes'
import { copyPlainText } from '../clipboard'
import { parsePatch, patchStats } from '../diff'
import { basename, formatBytes } from '../format'
import { highlightLines, languageOfPath } from '../highlight'
import { renderMarkdown } from '../markdown'
import type { ChangeFile, ChangeReport, FileDiff, FileHit, FileText } from '../types'

const props = defineProps<{
  /** 会话的工作目录；null＝不显示。 */
  directory: string | null
  /** 现在有没有回合在跑：跑完自动刷一次（看新的改动）。 */
  running?: boolean
  /** 打开时直接定位到这个文件（聊天里点路径进来的）。绝对路径。 */
  initialPath?: string | null
}>()

const emit = defineEmits<{
  (e: 'close'): void
}>()

type Mode = 'diff' | 'preview' | 'edit'

/** 左边列什么：本次改动 / 项目全部文件。 */
const listMode = ref<'changes' | 'all'>('changes')

const report = ref<ChangeReport | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

const allFiles = ref<FileHit[]>([])
const allLoading = ref(false)
const allError = ref<string | null>(null)
const allQuery = ref('')

/** 当前看的文件（两种列表统一成这一份）。 */
const active = ref<{ rel: string; path: string } | null>(null)
const mode = ref<Mode>('diff')
const diff = ref<FileDiff | null>(null)
const text = ref<FileText | null>(null)
/** 图片预览的 data URL（png / svg 等）。 */
const image = ref<string | null>(null)
const detailLoading = ref(false)
const detailError = ref<string | null>(null)
const menuOpen = ref(false)

/** 编辑器状态。 */
const draft = ref('')
const saving = ref(false)
const saveNote = ref<string | null>(null)
/** mode 是被「这个文件没有 diff」逼成预览的：等改动数据到位再切回 diff。 */
let modeForced = false

const files = computed<ChangeFile[]>(() => report.value?.files ?? [])
const activeChange = computed(
  () => files.value.find(file => file.rel === active.value?.rel) ?? null,
)
const canDiff = computed(() => !!activeChange.value)
const kind = computed(() => fileKindOf(active.value?.rel))
const canEdit = computed(() => kind.value !== 'image' && !!text.value && !text.value.binary)
const diffRows = computed(() => (diff.value ? parsePatch(diff.value.patch) : []))
const diffStats = computed(() => patchStats(diffRows.value))
const changedRels = computed(() => new Set(files.value.map(file => file.rel)))
const allView = computed(() => filterProjectFiles(allFiles.value, allQuery.value))
const dirty = computed(() => !!text.value && !text.value.binary && draft.value !== text.value.text)

/** 代码预览：一行一条，带行号与高亮（markdown / 图片不走这里）。 */
const codeLines = computed(() => {
  const lines = numberedLines(text.value?.text ?? '')
  if (!lines.length) return []
  const html = highlightLines(text.value?.text ?? '', languageOfPath(active.value?.rel))
  return lines.map((line, index) => ({ no: line.no, html: html[index] ?? '' }))
})

const markdownHtml = computed(() =>
  kind.value === 'markdown' && text.value && !text.value.binary
    ? renderMarkdown(text.value.text)
    : '',
)

// ---- 数据 ----

/**
 * 选中文件后决定看哪一屏：
 * 有改动看 diff；没有就退到预览；之前被逼退过、现在又有改动了，切回 diff。
 */
function ensureMode() {
  if (!canDiff.value) {
    // 只把「变更」这一屏退掉，用户自己选的预览 / 编辑不动。
    if (mode.value === 'diff') {
      mode.value = 'preview'
      modeForced = true
    }
    return
  }
  if (modeForced) {
    mode.value = 'diff'
    modeForced = false
  }
}

async function load() {
  const directory = props.directory
  if (!directory) return
  loading.value = true
  error.value = null
  try {
    const loaded = await api.changesList(directory)
    report.value = loaded
    // 选中的文件可能不在改动里（本来就是干净的、或被提交了）：保留选中——
    // 用户还在看这个文件，只是「变更」这一屏要退到预览。
    if (active.value) ensureMode()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    loading.value = false
  }
}

async function loadAllFiles() {
  const directory = props.directory
  if (!directory || allLoading.value) return
  allLoading.value = true
  allError.value = null
  try {
    allFiles.value = await api.projectFiles(directory, '', 4000)
  } catch (cause) {
    allError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    allLoading.value = false
  }
}

async function loadDetail() {
  const directory = props.directory
  const file = active.value
  if (!directory || !file) {
    diff.value = null
    text.value = null
    image.value = null
    return
  }
  detailLoading.value = true
  detailError.value = null
  saveNote.value = null
  try {
    if (mode.value === 'diff' && canDiff.value) {
      diff.value = await api.changesDiff(directory, file.rel)
      text.value = null
      image.value = null
      return
    }

    diff.value = null

    // 图片：交给交付物读取那条路（base64 / svg 文本），不解码成乱码文本。
    if (kind.value === 'image') {
      const data = await api.readArtifact(file.path)
      if (data.kind === 'image' && data.base64) {
        image.value = `data:${data.mime};base64,${data.base64}`
      } else if (data.kind === 'svg' && data.text) {
        image.value = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(data.text)}`
      } else {
        image.value = null
        detailError.value = '这个图片格式不支持预览'
      }
      text.value = null
      draft.value = ''
      return
    }

    image.value = null
    const data = await api.fileText(file.path)
    text.value = data
    draft.value = data.binary ? '' : data.text
    if (data.binary && mode.value === 'edit') mode.value = 'preview'
  } catch (cause) {
    detailError.value = cause instanceof Error ? cause.message : String(cause)
    diff.value = null
    text.value = null
    image.value = null
  } finally {
    detailLoading.value = false
  }
}

// ---- 交互 ----

function pickChange(file: ChangeFile) {
  active.value = { rel: file.rel, path: file.path }
  ensureMode()
}

/** 绝对路径 → 抽屉里的选中项（在仓库内就换算成 rel，外面就当普通文件预览）。 */
function openAbsolute(path: string) {
  const directory = props.directory ?? ''
  const clean = directory.replace(/\/+$/, '')
  const rel = clean && path.startsWith(`${clean}/`) ? path.slice(clean.length + 1) : path
  active.value = { rel, path }
  ensureMode()
}

function pickAny(hit: FileHit) {
  const directory = props.directory ?? ''
  active.value = { rel: hit.path, path: joinPath(directory, hit.path) }
  // 全部文件里点开：改过的看变更，没改的看内容。
  ensureMode()
}

function setList(next: 'changes' | 'all') {
  listMode.value = next
  if (next === 'all' && !allFiles.value.length) void loadAllFiles()
}

function setMode(next: Mode) {
  if (next === 'diff' && !canDiff.value) return
  if (next === 'edit' && !canEdit.value) return
  mode.value = next
  modeForced = false
}

async function save() {
  const file = active.value
  const directory = props.directory
  if (!file || !directory || saving.value || !dirty.value) return
  saving.value = true
  detailError.value = null
  try {
    await api.writeText(file.path, draft.value, directory)
    if (text.value) text.value = { ...text.value, text: draft.value, size: draft.value.length }
    saveNote.value = '已保存'
    window.setTimeout(() => {
      if (saveNote.value === '已保存') saveNote.value = null
    }, 1600)
    // 存完这个文件就变成「已改动」了，列表得跟着更新。
    await load()
  } catch (cause) {
    detailError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    saving.value = false
  }
}

function openExternally() {
  const file = active.value
  if (file) void api.openPath(file.path).catch(() => undefined)
}

function reveal() {
  const file = active.value
  if (file) void api.revealPath(file.path).catch(() => undefined)
}

function copyPath() {
  const file = active.value
  if (file) void copyPlainText(file.path)
  menuOpen.value = false
}

watch(
  () => props.initialPath,
  path => {
    if (path) openAbsolute(path)
  },
)
watch(() => active.value?.rel, () => void loadDetail())
watch(mode, () => void loadDetail())
watch(
  () => props.directory,
  () => {
    active.value = null
    allFiles.value = []
    void load()
  },
)
// 一轮跑完刷新一次：改动的文件数 / 行数会变。
watch(
  () => props.running,
  (running, was) => {
    if (was && !running) void load()
  },
)

function onKeydown(event: KeyboardEvent) {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's' && mode.value === 'edit') {
    event.preventDefault()
    void save()
    return
  }
  if (event.key !== 'Escape') return
  event.stopPropagation()
  if (menuOpen.value) {
    menuOpen.value = false
    return
  }
  emit('close')
}

onMounted(() => {
  window.addEventListener('keydown', onKeydown)
  void load()
  if (props.initialPath) openAbsolute(props.initialPath)
})

onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div class="changes-backdrop" @click="emit('close')" />
  <aside class="changes-drawer" aria-label="文件">
    <header class="ch-head">
      <div class="ch-head-main">
        <span class="ch-title">文件</span>
        <span v-if="listMode === 'changes' && report?.isRepo && files.length" class="ch-counts">
          <span class="ch-added">+{{ report.additions }}</span>
          <span class="ch-removed">−{{ report.deletions }}</span>
          <span class="ch-files">{{ files.length }} 个文件</span>
        </span>
        <span v-else-if="listMode === 'all' && allFiles.length" class="ch-counts ch-files">
          {{ allFiles.length }} 个文件
        </span>
        <span v-if="report?.branch" class="ch-branch" :title="report.branch">
          {{ report.branch }}
        </span>
      </div>
      <div class="ch-head-acts">
        <button class="ch-icon" title="刷新" :disabled="loading" @click="load">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20 11a8 8 0 1 1-2.3-5.6" />
            <path d="M20 4v7h-7" />
          </svg>
        </button>
        <button class="ch-icon" title="关闭（Esc）" @click="emit('close')">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" aria-hidden="true">
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
      </div>
    </header>

    <div class="ch-body">
      <div class="ch-side">
        <div class="ch-switch">
          <button class="ch-switch-btn" :class="{ on: listMode === 'changes' }" @click="setList('changes')">
            变更<span v-if="files.length" class="ch-switch-count">{{ files.length }}</span>
          </button>
          <button class="ch-switch-btn" :class="{ on: listMode === 'all' }" @click="setList('all')">
            全部文件<span v-if="allFiles.length" class="ch-switch-count">{{ allFiles.length }}</span>
          </button>
        </div>

        <!-- 变更 -->
        <template v-if="listMode === 'changes'">
          <p v-if="error" class="ch-error">{{ error }}</p>
          <p v-else-if="loading && !report" class="ch-hint">正在读改动…</p>
          <p v-else-if="report && !report.isRepo" class="ch-hint">
            这个目录不在 Git 仓库里，看不到变更；「全部文件」照样能翻。
          </p>
          <p v-else-if="report && !files.length" class="ch-hint">
            工作树是干净的——这个会话没有留下未提交的改动。
          </p>
          <ul v-else class="ch-list">
            <li v-for="file in files" :key="file.rel">
              <button
                class="ch-row"
                :class="{ on: file.rel === active?.rel }"
                :title="`${statusLabel(file.status)}：${file.rel}`"
                @click="pickChange(file)"
              >
                <span class="ch-mark" :class="`st-${file.status}`">{{ statusMark(file.status) }}</span>
                <span class="ch-path">
                  <span class="ch-dir">{{ splitPath(file.rel).dir }}</span>
                  <span class="ch-name">{{ splitPath(file.rel).name }}</span>
                </span>
                <span class="ch-stat">
                  <span v-if="file.additions" class="ch-added">+{{ file.additions }}</span>
                  <span v-if="file.deletions" class="ch-removed">−{{ file.deletions }}</span>
                  <span v-if="file.binary" class="ch-binary">bin</span>
                </span>
              </button>
            </li>
          </ul>
        </template>

        <!-- 全部文件 -->
        <template v-else>
          <div class="ch-search">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" aria-hidden="true">
              <circle cx="11" cy="11" r="6" />
              <path d="m20 20-3.6-3.6" />
            </svg>
            <input v-model="allQuery" type="text" placeholder="按文件名 / 路径筛选" aria-label="筛选文件" />
          </div>
          <p v-if="allError" class="ch-error">{{ allError }}</p>
          <p v-else-if="allLoading && !allFiles.length" class="ch-hint">正在列文件…</p>
          <p v-else-if="!allView.items.length" class="ch-hint">
            {{ allQuery ? '没有匹配的文件' : '这个目录里没有可列的文件' }}
          </p>
          <ul v-else class="ch-list">
            <li v-for="hit in allView.items" :key="hit.path">
              <button
                class="ch-row"
                :class="{ on: hit.path === active?.rel }"
                :title="hit.path"
                @click="pickAny(hit)"
              >
                <span
                  class="ch-mark"
                  :class="changedRels.has(hit.path) ? `st-${files.find(file => file.rel === hit.path)?.status}` : 'st-plain'"
                >
                  {{ changedRels.has(hit.path) ? statusMark(files.find(file => file.rel === hit.path)!.status) : '·' }}
                </span>
                <span class="ch-path">
                  <span class="ch-dir">{{ splitPath(hit.path).dir }}</span>
                  <span class="ch-name">{{ hit.name }}</span>
                </span>
              </button>
            </li>
          </ul>
          <p v-if="allView.total > allView.items.length" class="ch-more">
            还有 {{ allView.total - allView.items.length }} 个没列出来，输入关键词缩小范围。
          </p>
        </template>
      </div>

      <section class="ch-detail">
        <template v-if="active">
          <header class="ch-detail-head">
            <span class="ch-detail-path" :title="active.rel">{{ active.rel }}</span>
            <div class="ch-tabs">
              <button
                class="ch-tab"
                :class="{ on: mode === 'diff' }"
                :disabled="!canDiff"
                :title="canDiff ? '看这个文件的改动' : '这个文件没有未提交改动'"
                @click="setMode('diff')"
              >
                变更
              </button>
              <button class="ch-tab" :class="{ on: mode === 'preview' }" @click="setMode('preview')">
                {{ kind === 'markdown' ? '预览' : '内容' }}
              </button>
              <button
                class="ch-tab"
                :class="{ on: mode === 'edit' }"
                :disabled="!canEdit"
                :title="canEdit ? '直接改，⌘S 保存' : '这个文件不能在这里编辑'"
                @click="setMode('edit')"
              >
                编辑
              </button>
            </div>
            <div class="ch-open-wrap">
              <button class="ch-open" :aria-expanded="menuOpen" @click.stop="menuOpen = !menuOpen">
                打开方式
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <path d="m6 9 6 6 6-6" />
                </svg>
              </button>
              <template v-if="menuOpen">
                <div class="ch-menu-backdrop" @click.stop="menuOpen = false" />
                <div class="ch-menu">
                  <button class="ch-menu-item" @click="setMode('preview'); menuOpen = false">
                    预览
                  </button>
                  <button class="ch-menu-item" :disabled="!canDiff" @click="setMode('diff'); menuOpen = false">
                    看变更
                  </button>
                  <button class="ch-menu-item" :disabled="!canEdit" @click="setMode('edit'); menuOpen = false">
                    编辑
                  </button>
                  <div class="ch-menu-sep" />
                  <button class="ch-menu-item" @click="openExternally(); menuOpen = false">
                    默认应用打开
                  </button>
                  <button class="ch-menu-item" @click="reveal(); menuOpen = false">
                    在访达中显示
                  </button>
                  <button class="ch-menu-item" @click="copyPath">复制路径</button>
                </div>
              </template>
            </div>
          </header>

          <div class="ch-detail-body" :class="{ 'is-edit': mode === 'edit' }">
            <p v-if="detailLoading" class="ch-hint">正在读…</p>
            <p v-else-if="detailError" class="ch-error">{{ detailError }}</p>

            <!-- 变更 -->
            <template v-else-if="mode === 'diff'">
              <p v-if="diff?.binary" class="ch-hint">二进制文件，没有文本 diff。</p>
              <p v-else-if="diff && !diff.patch.trim()" class="ch-hint">这个文件没有可显示的差异。</p>
              <template v-else-if="diff">
                <div class="ch-diff-meta">
                  <span class="ch-added">+{{ diffStats.added }}</span>
                  <span class="ch-removed">−{{ diffStats.removed }}</span>
                  <span v-if="diff.truncated" class="ch-trunc">差异过大，只显示前一段</span>
                </div>
                <div class="ch-diff">
                  <div
                    v-for="(row, index) in diffRows"
                    :key="index"
                    class="ch-drow"
                    :class="`t-${row.type}`"
                  >
                    <span v-if="row.type !== 'hunk'" class="ch-sign">
                      {{ row.type === 'add' ? '+' : row.type === 'del' ? '−' : ' ' }}
                    </span>
                    <span class="ch-line-text">{{ row.text }}</span>
                  </div>
                </div>
              </template>
            </template>

            <!-- 编辑 -->
            <div v-else-if="mode === 'edit'" class="ch-edit">
              <textarea
                v-model="draft"
                class="ch-editor"
                spellcheck="false"
                autocapitalize="off"
                autocomplete="off"
                :readonly="saving"
              />
              <div class="ch-edit-bar">
                <span class="ch-edit-note" :class="{ ok: saveNote === '已保存' }">
                  {{ saveNote ?? (dirty ? '有未保存的修改' : '与磁盘一致') }}
                </span>
                <button class="ch-save" :disabled="!dirty || saving" @click="save">
                  {{ saving ? '保存中…' : '保存' }}
                </button>
              </div>
            </div>

            <!-- 预览：图片 / markdown / 代码 -->
            <template v-else>
              <div v-if="kind === 'image'" class="ch-image">
                <img v-if="image" :src="image" :alt="active.rel" />
                <p v-else class="ch-hint">这个图片读不出来。</p>
              </div>
              <div
                v-else-if="kind === 'markdown' && markdownHtml"
                class="md ch-md"
                v-html="markdownHtml"
              />
              <p v-else-if="text?.binary" class="ch-hint">
                {{ basename(text.path) }} 是二进制文件（{{ formatBytes(text.size) }}），不做文本预览。
              </p>
              <template v-else-if="text">
                <p v-if="text.truncated" class="ch-trunc">
                  文件 {{ formatBytes(text.size) }}，只显示前一段。
                </p>
                <div class="ch-code">
                  <div v-for="line in codeLines" :key="line.no" class="ch-code-row">
                    <span class="ch-code-no">{{ line.no }}</span>
                    <span class="ch-code-text" v-html="line.html || ' '" />
                  </div>
                  <p v-if="!codeLines.length" class="ch-hint">空文件。</p>
                </div>
              </template>
            </template>
          </div>
        </template>

        <div v-else class="ch-hint ch-detail-empty">
          左边挑一个文件：改过的看 diff，没改的看内容。
        </div>
      </section>
    </div>
  </aside>
</template>

<style scoped src="../styles/components/ChangesPane.css"></style>
