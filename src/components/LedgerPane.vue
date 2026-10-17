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

const files = computed(() => {
  const list = ledger.value?.files ?? []
  return changedOnly.value ? list.filter(isChanged) : list
})

const hiddenReads = computed(() => {
  const list = ledger.value?.files ?? []
  return changedOnly.value ? list.length - files.value.length : 0
})

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
      <span v-if="ledger" class="ledger-count">{{ ledger.changedFiles }} 个文件有改动</span>
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
      <span v-if="hiddenReads" class="ledger-hidden">隐藏 {{ hiddenReads }} 个只读文件</span>
      <span class="ledger-sum">
        {{ ledger.totalEvents }} 次操作 · <span class="delta-plus">+{{ totals.added }}</span>
        <span class="delta-minus">−{{ totals.removed }}</span>
      </span>
    </div>

    <div class="ledger-body">
      <div v-if="loading && !ledger" class="ledger-empty">扫描中…</div>
      <div v-else-if="error" class="ledger-empty">{{ error }}</div>
      <div v-else-if="!files.length" class="ledger-empty">
        {{ changedOnly ? '这个会话没有改过文件' : '这个会话还没有文件操作' }}
      </div>
      <template v-else>
        <div v-for="file in files" :key="file.path" class="ledger-file">
          <button class="file-head" :title="file.path" @click="toggle(file.path)">
            <span class="file-caret">{{ expanded.has(file.path) ? '▾' : '▸' }}</span>
            <span class="file-main">
              <span class="file-name">{{ splitPath(file.path).name }}</span>
              <span v-if="splitPath(file.path).dir" class="file-dir">
                {{ splitPath(file.path).dir }}
              </span>
            </span>
            <span class="file-badges">
              <span v-if="file.added" class="delta-plus">+{{ file.added }}</span>
              <span v-if="file.removed" class="delta-minus">−{{ file.removed }}</span>
              <span v-if="!file.added && !file.removed" class="file-times">
                {{ file.events.length }} 次
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
        <div v-if="ledger?.truncated" class="ledger-warn">操作过多，仅展示最近的部分。</div>
      </template>
    </div>

    <footer class="ledger-foot">点某次操作可跳回时间线对应轮次</footer>
  </aside>
</template>

<style scoped src="../styles/components/LedgerPane.css"></style>
