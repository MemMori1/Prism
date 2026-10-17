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

<style scoped>
.ledger {
  position: absolute;
  top: 0;
  right: 0;
  bottom: 0;
  z-index: 55;
  width: 352px;
  display: flex;
  flex-direction: column;
  background: var(--panel);
  border-left: 1px solid var(--border);
  box-shadow: -14px 0 32px rgb(18 16 34 / 0.1);
}

.ledger-head {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 12px 10px 14px;
  border-bottom: 1px solid var(--border);
}

.ledger-title {
  font-size: 13px;
  font-weight: 650;
}

.ledger-count {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--faint);
  font-size: 10.5px;
}

.ledger-icon {
  flex: none;
  width: 24px;
  height: 24px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 11px;
  cursor: pointer;
}

.ledger-icon:hover:not(:disabled) {
  color: var(--text);
  border-color: var(--border-strong);
}

.ledger-icon:disabled {
  opacity: 0.55;
  cursor: default;
}

.ledger-stats {
  flex: none;
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px 10px;
  padding: 8px 14px;
  border-bottom: 1px solid var(--border);
  color: var(--muted);
  font-size: 11px;
}

.ledger-toggle {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  cursor: pointer;
  user-select: none;
}

.ledger-hidden {
  color: var(--faint);
  font-size: 10.5px;
}

.ledger-sum {
  margin-left: auto;
  font-family: var(--font-mono);
  font-size: 10.5px;
  white-space: nowrap;
}

.ledger-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 6px 8px 12px;
}

.ledger-empty {
  padding: 26px 12px;
  text-align: center;
  color: var(--faint);
  font-size: 12px;
}

.ledger-file + .ledger-file {
  margin-top: 2px;
}

.file-head {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 6px;
  border-radius: var(--radius-sm);
  text-align: left;
  cursor: pointer;
}

.file-head:hover {
  background: var(--panel-2);
}

.file-caret {
  flex: none;
  width: 12px;
  color: var(--faint);
  font-size: 10px;
}

.file-main {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: baseline;
  gap: 6px;
}

.file-name {
  font-size: 12px;
  font-weight: 560;
  white-space: nowrap;
}

.file-dir {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--faint);
  font-size: 10.5px;
}

.file-badges {
  flex: none;
  display: inline-flex;
  gap: 5px;
  font-family: var(--font-mono);
  font-size: 10.5px;
}

.file-times {
  color: var(--faint);
}

.delta-plus {
  color: var(--green);
}

.delta-minus {
  color: var(--red);
}

.file-events {
  margin: 2px 0 6px 18px;
  padding-left: 8px;
  border-left: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.event-row {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 3px 6px;
  border-radius: 5px;
  text-align: left;
  cursor: pointer;
}

.event-row:hover {
  background: var(--panel-2);
}

.event-time {
  flex: none;
  color: var(--faint);
  font-size: 10px;
  font-family: var(--font-mono);
}

.event-kind {
  flex: none;
  width: 15px;
  text-align: center;
  border-radius: 4px;
  font-size: 9.5px;
  line-height: 14px;
  border: 1px solid var(--border);
  color: var(--muted);
}

.event-kind.kind-edit {
  color: var(--amber);
  border-color: color-mix(in srgb, var(--amber) 45%, var(--border));
}

.event-kind.kind-write {
  color: var(--green);
  border-color: color-mix(in srgb, var(--green) 45%, var(--border));
}

.event-kind.kind-delete {
  color: var(--red);
  border-color: color-mix(in srgb, var(--red) 45%, var(--border));
}

.event-tool {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  font-size: 10.5px;
  font-family: var(--font-mono);
}

.event-delta {
  flex: none;
  display: inline-flex;
  gap: 5px;
  font-family: var(--font-mono);
  font-size: 10px;
}

.ledger-warn {
  padding: 10px;
  color: var(--amber);
  font-size: 10.5px;
}

.ledger-foot {
  flex: none;
  padding: 8px 14px;
  border-top: 1px solid var(--border);
  color: var(--faint);
  font-size: 10.5px;
}
</style>
