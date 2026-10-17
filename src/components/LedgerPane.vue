<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import type { LedgerFile, SessionLedger } from '../types'

const props = defineProps<{
  sessionId: string
  directory: string
}>()

const emit = defineEmits<{
  (e: 'jump', recordId: string): void
  (e: 'close'): void
}>()

const ledger = ref<SessionLedger | null>(null)
const loading = ref(false)
const error = ref('')
/** 只看有改动的文件（默认开：只读文件常占大头）。 */
const changedOnly = ref(true)
const expanded = ref(new Set<string>())

async function load() {
  loading.value = true
  error.value = ''
  try {
    ledger.value = await api.sessionLedger(props.sessionId)
  } catch (cause) {
    error.value = String(cause)
  } finally {
    loading.value = false
  }
}

onMounted(load)

watch(
  () => props.sessionId,
  () => {
    ledger.value = null
    expanded.value = new Set()
    void load()
  },
)

function isChanged(file: LedgerFile): boolean {
  return file.events.some(event => event.kind !== 'read')
}

const changedList = computed(() => (ledger.value?.files ?? []).filter(isChanged))
const readList = computed(() => (ledger.value?.files ?? []).filter(file => !isChanged(file)))

/** 只看改动时只有一段；看全部时把只读文件沉到「只读」段，不和改动混在一起。 */
const sections = computed(() => {
  if (changedOnly.value) {
    return [{ key: 'changed', label: '', files: changedList.value }]
  }
  return [
    { key: 'changed', label: '有改动', files: changedList.value },
    { key: 'read', label: '只读', files: readList.value },
  ].filter(section => section.files.length > 0)
})

const hiddenReads = computed(() => (changedOnly.value ? readList.value.length : 0))

/** 真正改过文件的次数（只读不算）。 */
function changeCount(file: LedgerFile): number {
  return file.events.filter(event => event.kind !== 'read').length
}

/** 相对目录（第二行左侧，可省略）。 */
function dirOf(file: LedgerFile): string {
  return splitPath(file.path).dir
}

/** 最近操作时间：今天只显示时分。 */
function shortTime(file: LedgerFile): string {
  if (!file.lastTime) return ''
  const day = dayLabel(file.lastTime)
  return day === '今天' ? clock(file.lastTime) : `${day} ${clock(file.lastTime)}`
}

const totals = computed(() => {
  let added = 0
  let removed = 0
  for (const file of ledger.value?.files ?? []) {
    added += file.added
    removed += file.removed
  }
  return { added, removed }
})

/** 相对会话工作目录的显示路径（前缀不匹配时原样显示）。 */
function relative(raw: string): string {
  const prefix = props.directory.endsWith('/') ? props.directory : `${props.directory}/`
  return raw.startsWith(prefix) ? raw.slice(prefix.length) : raw
}

function splitPath(raw: string): { name: string; dir: string } {
  const path = relative(raw)
  const index = path.lastIndexOf('/')
  if (index === -1) return { name: path, dir: '' }
  return { name: path.slice(index + 1), dir: path.slice(0, index) }
}

function clock(ms: number): string {
  if (!ms) return '--:--'
  const date = new Date(ms)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`
}

function dayLabel(ms: number): string {
  if (!ms) return ''
  const date = new Date(ms)
  const now = new Date()
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  if (sameDay(date, now)) return '今天'
  if (sameDay(date, new Date(now.getTime() - 86_400_000))) return '昨天'
  return `${date.getMonth() + 1}-${date.getDate()}`
}

const KIND_LABELS: Record<string, string> = {
  read: '读',
  edit: '改',
  write: '写',
  delete: '删',
}

function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind
}

function toggle(path: string) {
  const next = new Set(expanded.value)
  if (next.has(path)) {
    next.delete(path)
  } else {
    next.add(path)
  }
  expanded.value = next
}
</script>

<template>
  <aside class="ledger">
    <header class="ledger-head">
      <span class="ledger-title">文件账本</span>
      <span v-if="ledger" class="ledger-count">
        {{ ledger.changedFiles }} 个文件有改动 · {{ ledger.totalEvents }} 次操作
      </span>
      <button class="ledger-icon" title="重新扫描" :disabled="loading" @click="load">
        {{ loading ? '…' : '⟳' }}
      </button>
      <button class="ledger-icon" title="关闭" @click="emit('close')">✕</button>
    </header>

    <div v-if="ledger" class="ledger-stats">
      <label class="ledger-toggle">
        <input v-model="changedOnly" type="checkbox" />
        只看改动
      </label>
      <span v-if="hiddenReads" class="ledger-hidden">另有 {{ hiddenReads }} 个只读文件</span>
      <span class="ledger-sum" title="所有文件历次操作的增删行数之和，不是净增行数">
        累计
        <span class="delta-plus">+{{ totals.added }}</span>
        <span class="delta-minus">−{{ totals.removed }}</span>
      </span>
    </div>

    <div class="ledger-body">
      <div v-if="loading && !ledger" class="ledger-empty">扫描中…</div>
      <div v-else-if="error" class="ledger-empty">{{ error }}</div>
      <div v-else-if="!changedList.length && !readList.length" class="ledger-empty">
        这个会话还没有文件操作
      </div>
      <div v-else-if="changedOnly && !changedList.length" class="ledger-empty">
        这个会话没有改过文件<br />
        <span class="ledger-hint">
          取消上面「只看改动」可以看到 {{ readList.length }} 个只读文件
        </span>
      </div>
      <template v-else>
        <div v-for="section in sections" :key="section.key" class="ledger-group">
          <div v-if="section.label" class="ledger-section">
            <span>{{ section.label }}</span>
            <span class="ledger-section-count">{{ section.files.length }}</span>
          </div>

          <div v-for="file in section.files" :key="file.path" class="ledger-file">
            <button
              class="file-head"
              :class="{ 'is-read': !isChanged(file) }"
              :title="`${file.path}\n${isChanged(file) ? `改动 ${changeCount(file)} 次` : '只读'}；+ / − 为历次操作的累计增删行数，不是净增行数`"
              @click="toggle(file.path)"
            >
              <span class="file-caret">{{ expanded.has(file.path) ? '▾' : '▸' }}</span>
              <span class="file-main">
                <span class="file-line">
                  <span class="file-name">{{ splitPath(file.path).name }}</span>
                  <span class="file-count">
                    {{ isChanged(file) ? `${changeCount(file)} 次改动` : '只读' }}
                  </span>
                </span>
                <span class="file-line">
                  <span class="file-dir">{{ dirOf(file) }}</span>
                  <span class="file-time">{{ shortTime(file) }}</span>
                  <span
                    v-if="file.added || file.removed"
                    class="file-sum"
                    title="历次操作的累计增删行数，不是净增行数"
                  >
                    累计 <span class="delta-plus">+{{ file.added }}</span>
                    <span class="delta-minus">−{{ file.removed }}</span>
                  </span>
                </span>
              </span>
            </button>

            <div v-if="expanded.has(file.path)" class="file-events">
              <button
                v-for="(event, index) in file.events"
                :key="`${event.recordId}-${index}`"
                class="event-row"
                :title="`跳到时间线：${event.tool}`"
                @click="emit('jump', event.recordId)"
              >
                <span class="event-time">{{ dayLabel(event.time) }} {{ clock(event.time) }}</span>
                <span class="event-kind" :class="`kind-${event.kind}`">
                  {{ kindLabel(event.kind) }}
                </span>
                <span class="event-tool">{{ event.tool }}</span>
                <span class="event-delta">
                  <span v-if="event.added" class="delta-plus">+{{ event.added }}</span>
                  <span v-if="event.removed" class="delta-minus">−{{ event.removed }}</span>
                </span>
              </button>
            </div>
          </div>
        </div>
        <div v-if="ledger?.truncated" class="ledger-warn">操作过多，仅展示最近的部分。</div>
      </template>
    </div>

    <footer class="ledger-foot">
      点某次操作可跳回时间线对应轮次 · 行尾「累计」是历次增删之和
    </footer>
  </aside>
</template>

<style scoped src="../styles/components/LedgerPane.css"></style>
