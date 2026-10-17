<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../api'
import { renderMarkdown } from '../markdown'
import { relativeTime } from '../format'
import { commandLine, engineLabel, groupAssets, groupMcp, kindLabel, scopeLabel } from '../workshop'
import type { AssetGroup } from '../workshop'
import type { AssetFile, AssetItem, McpProbeResult, McpServer } from '../types'

const emit = defineEmits<{
  (e: 'error', message: string): void
}>()

const items = ref<AssetItem[]>([])
const loading = ref(false)
const loaded = ref(false)
const activeKey = ref('')
const selected = ref<AssetItem | null>(null)
const fileContent = ref<AssetFile | null>(null)
const fileLoading = ref(false)
const fileError = ref('')
const filterText = ref('')

type PanelMode = 'assets' | 'mcp'
const mode = ref<PanelMode>('assets')

// ---- MCP 模式状态 ----
const mcpServers = ref<McpServer[]>([])
const mcpLoading = ref(false)
const mcpLoaded = ref(false)
const mcpEngine = ref('')
const mcpSelected = ref<McpServer | null>(null)
const probes = ref<Record<string, { state: 'probing' } | { state: 'done'; result: McpProbeResult }>>(
  {},
)

const mcpGroups = computed(() => groupMcp(mcpServers.value))
const mcpItems = computed(
  () => mcpGroups.value.find((group) => group.engine === mcpEngine.value)?.items ?? [],
)
const visibleMcp = computed(() => {
  const term = filterText.value.trim().toLowerCase()
  if (!term) {
    return mcpItems.value
  }
  return mcpItems.value.filter(
    (server) =>
      server.name.toLowerCase().includes(term) ||
      (server.url ?? '').toLowerCase().includes(term) ||
      (server.command ?? '').toLowerCase().includes(term),
  )
})

const groups = computed(() => groupAssets(items.value))

/** 左侧导航：按引擎分段，段内按种类。 */
const navSections = computed(() => {
  const sections: { engine: string; label: string; groups: AssetGroup[] }[] = []
  for (const group of groups.value) {
    const last = sections[sections.length - 1]
    if (last && last.engine === group.engine) {
      last.groups.push(group)
    } else {
      sections.push({ engine: group.engine, label: engineLabel(group.engine), groups: [group] })
    }
  }
  return sections
})

const activeGroup = computed(
  () => groups.value.find((group) => group.key === activeKey.value) ?? null,
)

const visibleItems = computed(() => {
  const group = activeGroup.value
  if (!group) {
    return []
  }
  const term = filterText.value.trim().toLowerCase()
  if (!term) {
    return group.items
  }
  return group.items.filter(
    (entry) =>
      entry.name.toLowerCase().includes(term) ||
      (entry.description ?? '').toLowerCase().includes(term),
  )
})

const isMarkdown = computed(() => selected.value?.path.toLowerCase().endsWith('.md') ?? false)

const previewHtml = computed(() => {
  const file = fileContent.value
  return file ? renderMarkdown(file.content) : ''
})

function report(cause: unknown): void {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

async function load(): Promise<void> {
  loading.value = true
  try {
    items.value = await api.workshopAssets()
    loaded.value = true
    const first = groups.value[0]
    if (first) {
      selectGroup(first.key)
    } else {
      activeKey.value = ''
      selected.value = null
      fileContent.value = null
    }
  } catch (cause) {
    report(cause)
  } finally {
    loading.value = false
  }
}

function selectGroup(key: string): void {
  activeKey.value = key
  filterText.value = ''
  const first = groups.value.find((group) => group.key === key)?.items[0]
  if (first) {
    void selectItem(first)
  } else {
    selected.value = null
    fileContent.value = null
  }
}

async function selectItem(item: AssetItem): Promise<void> {
  selected.value = item
  fileContent.value = null
  fileError.value = ''
  fileLoading.value = true
  try {
    fileContent.value = await api.workshopRead(item.path)
  } catch (cause) {
    fileError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    fileLoading.value = false
  }
}

async function openExternal(path?: string): Promise<void> {
  const target = path ?? selected.value?.path
  if (!target) {
    return
  }
  try {
    await api.openPath(target)
  } catch (cause) {
    report(cause)
  }
}

// ---- MCP 模式 ----

function switchMode(next: PanelMode): void {
  if (mode.value === next) {
    return
  }
  mode.value = next
  filterText.value = ''
  if (next === 'mcp') {
    void loadMcp()
  }
}

function refresh(): void {
  if (mode.value === 'assets') {
    void load()
  } else {
    void loadMcp(true)
  }
}

async function loadMcp(force = false): Promise<void> {
  if (mcpLoading.value || (mcpLoaded.value && !force)) {
    return
  }
  mcpLoading.value = true
  try {
    mcpServers.value = await api.workshopMcp()
    mcpLoaded.value = true
    const first = mcpGroups.value[0]
    mcpEngine.value = first?.engine ?? ''
    mcpSelected.value = first?.items[0] ?? null
    for (const server of mcpServers.value) {
      if (server.transport === 'http' && server.url) {
        void probeServer(server)
      }
    }
  } catch (cause) {
    report(cause)
  } finally {
    mcpLoading.value = false
  }
}

function selectMcpEngine(engine: string): void {
  mcpEngine.value = engine
  mcpSelected.value = mcpGroups.value.find((group) => group.engine === engine)?.items[0] ?? null
}

function selectMcp(server: McpServer): void {
  mcpSelected.value = server
}

async function probeServer(server: McpServer): Promise<void> {
  const url = server.url
  if (!url || probes.value[url]?.state === 'probing') {
    return
  }
  probes.value = { ...probes.value, [url]: { state: 'probing' } }
  try {
    const result = await api.mcpProbe(url)
    probes.value = { ...probes.value, [url]: { state: 'done', result } }
  } catch (cause) {
    probes.value = {
      ...probes.value,
      [url]: {
        state: 'done',
        result: {
          ok: false,
          status: null,
          latencyMs: 0,
          error: cause instanceof Error ? cause.message : String(cause),
        },
      },
    }
  }
}

/** 列表圆点语义：stdio=空心（不探活）/ idle / probing / ok / fail。 */
function probeState(server: McpServer): string {
  if (server.transport !== 'http' || !server.url) {
    return 'stdio'
  }
  const entry = probes.value[server.url]
  if (!entry) {
    return 'idle'
  }
  if (entry.state === 'probing') {
    return 'probing'
  }
  return entry.result.ok ? 'ok' : 'fail'
}

function probeSummary(server: McpServer): string {
  if (!server.url) {
    return ''
  }
  const entry = probes.value[server.url]
  if (!entry) {
    return '未探活'
  }
  if (entry.state === 'probing') {
    return '探活中…'
  }
  const result = entry.result
  return result.ok ? `HTTP ${result.status} · ${result.latencyMs}ms` : (result.error ?? '连不上')
}

onMounted(load)
</script>

<template>
  <div class="workshop-pane">
    <aside class="pane nav">
      <div class="pane-head">
        <div class="mode-switch">
          <button
            class="mode-btn"
            :class="{ active: mode === 'assets' }"
            title="引擎资产：技能 / 命令 / 子代理 / 记忆 / 插件"
            @click="switchMode('assets')"
          >
            资产
          </button>
          <button
            class="mode-btn"
            :class="{ active: mode === 'mcp' }"
            title="MCP 服务：三引擎配置真相位置（只读）+ http 探活"
            @click="switchMode('mcp')"
          >
            MCP
          </button>
        </div>
        <button
          class="ghost-btn"
          :disabled="loading || mcpLoading"
          title="重新扫描引擎配置"
          @click="refresh"
        >
          刷新
        </button>
      </div>
      <div class="pane-body">
        <template v-if="mode === 'assets'">
          <p v-if="loading && !loaded" class="hint">正在扫描引擎资产…</p>
          <p v-else-if="!groups.length" class="hint">
            还没有发现资产。在引擎的资产目录放置内容后点上方「刷新」：<br />
            <code>~/.codex/skills</code> · <code>~/.config/opencode/skills</code> ·
            <code>~/.claude/skills</code>，项目内 <code>.claude/</code>、<code>.opencode/</code>
          </p>
          <section v-for="section in navSections" :key="section.engine" class="nav-section">
            <div class="nav-engine">{{ section.label }}</div>
            <button
              v-for="group in section.groups"
              :key="group.key"
              class="nav-item"
              :class="{ active: group.key === activeKey }"
              @click="selectGroup(group.key)"
            >
              <span>{{ group.label }}</span>
              <span class="count">{{ group.items.length }}</span>
            </button>
          </section>
        </template>
        <template v-else>
          <p v-if="mcpLoading && !mcpLoaded" class="hint">正在读取 MCP 配置…</p>
          <p v-else-if="!mcpGroups.length" class="hint">暂无 MCP 服务</p>
          <section v-for="group in mcpGroups" :key="group.engine" class="nav-section">
            <div class="nav-engine">{{ group.label }}</div>
            <button
              class="nav-item"
              :class="{ active: group.engine === mcpEngine }"
              @click="selectMcpEngine(group.engine)"
            >
              <span>MCP 服务</span>
              <span class="count">{{ group.items.length }}</span>
            </button>
          </section>
        </template>
      </div>
    </aside>

    <aside class="pane list">
      <div class="pane-head">
        <input
          v-model="filterText"
          class="filter"
          placeholder="过滤名称 / 描述…"
          spellcheck="false"
        />
      </div>
      <div class="pane-body">
        <template v-if="mode === 'assets'">
          <p v-if="!activeGroup" class="hint">左侧选择一个分组</p>
          <p v-else-if="!visibleItems.length" class="hint">
            {{ filterText ? '没有匹配项' : '这个分组还没有资产' }}
          </p>
          <button
            v-for="entry in visibleItems"
            :key="entry.path"
            class="asset-row"
            :class="{ active: entry.path === selected?.path }"
            @click="selectItem(entry)"
          >
            <span class="asset-name">{{ entry.name }}</span>
            <span class="asset-meta">
              <span class="scope">{{ scopeLabel(entry) }}</span>
              <span>{{ relativeTime(entry.updatedAt) }}</span>
            </span>
            <span v-if="entry.description" class="asset-desc">{{ entry.description }}</span>
          </button>
        </template>
        <template v-else>
          <p v-if="mcpLoading && !mcpLoaded" class="hint">正在读取 MCP 配置…</p>
          <p v-else-if="!mcpGroups.length" class="hint">
            没有发现 MCP 服务。定义位置：OpenCode <code>opencode.jsonc</code> 的
            <code>mcp.servers</code>、Codex <code>config.toml</code> 的
            <code>[mcp_servers.*]</code>、Claude <code>~/.claude.json</code> 与项目
            <code>.mcp.json</code>；放置后点「刷新」。
          </p>
          <template v-else>
            <p v-if="!visibleMcp.length" class="hint">{{ filterText ? '没有匹配项' : '暂无服务' }}</p>
            <button
              v-for="entry in visibleMcp"
              :key="`${entry.source}:${entry.name}`"
              class="asset-row"
              :class="{ active: mcpSelected === entry }"
              @click="selectMcp(entry)"
            >
              <span class="asset-name">
                <span class="probe-dot" :class="probeState(entry)" />
                {{ entry.name }}
              </span>
              <span class="asset-meta">
                <span class="scope">{{ entry.transport === 'http' ? 'http' : 'stdio' }}</span>
                <span v-if="entry.disabled">已停用</span>
                <span>{{ scopeLabel(entry) }}</span>
              </span>
            </button>
          </template>
        </template>
      </div>
    </aside>

    <section class="pane detail">
      <template v-if="mode === 'assets' && selected">
        <div class="pane-head detail-head">
          <div class="detail-titles">
            <span class="detail-name">{{ selected.name }}</span>
            <span class="detail-path" :title="selected.path">{{ selected.path }}</span>
          </div>
          <button class="ghost-btn" title="用系统默认程序打开该文件（只读）" @click="openExternal()">
            打开文件
          </button>
        </div>
        <div class="detail-chips">
          <span class="chip">{{ engineLabel(selected.engine) }}</span>
          <span class="chip">{{ kindLabel(selected.kind) }}</span>
          <span class="chip">{{ scopeLabel(selected) }}</span>
          <span v-if="selected.project" class="chip mono" :title="selected.project">
            {{ selected.project }}
          </span>
        </div>
        <div class="pane-body detail-body">
          <p v-if="fileLoading" class="hint">读取中…</p>
          <p v-else-if="fileError" class="hint error">{{ fileError }}</p>
          <template v-else-if="fileContent">
            <div v-if="isMarkdown" class="md" v-html="previewHtml" />
            <pre v-else class="raw">{{ fileContent.content }}</pre>
            <p v-if="fileContent.truncated" class="hint">
              内容超出预览上限，已截断（点「打开文件」看全文）
            </p>
          </template>
        </div>
      </template>

      <template v-else-if="mode === 'mcp' && mcpSelected">
        <div class="pane-head detail-head">
          <div class="detail-titles">
            <span class="detail-name">{{ mcpSelected.name }}</span>
            <span class="detail-path" :title="mcpSelected.source">{{ mcpSelected.source }}</span>
          </div>
          <button
            class="ghost-btn"
            title="用系统默认程序打开该配置文件（只读）"
            @click="openExternal(mcpSelected.source)"
          >
            打开配置
          </button>
        </div>
        <div class="detail-chips">
          <span class="chip">{{ engineLabel(mcpSelected.engine) }}</span>
          <span class="chip">
            {{ mcpSelected.transport === 'http' ? 'HTTP（远端）' : 'stdio（本地进程）' }}
          </span>
          <span class="chip">{{ scopeLabel(mcpSelected) }}</span>
          <span v-if="mcpSelected.disabled" class="chip warn">已停用</span>
        </div>
        <div class="pane-body detail-body">
          <div class="mcp-field">
            <span class="field-label">
              {{ mcpSelected.transport === 'http' ? '地址' : '命令' }}
            </span>
            <code class="field-value">
              {{ mcpSelected.transport === 'http' ? mcpSelected.url : commandLine(mcpSelected) }}
            </code>
          </div>
          <div v-if="mcpSelected.transport === 'stdio'" class="mcp-field">
            <span class="field-label">命令状态</span>
            <span v-if="mcpSelected.commandExists === false" class="field-warn">
              找不到「{{ mcpSelected.command }}」——引擎启动该服务时可能失败
            </span>
            <span v-else-if="mcpSelected.commandExists === true" class="field-ok">命令可解析</span>
            <span v-else class="field-dim">未检查</span>
          </div>
          <div v-if="mcpSelected.envKeys.length" class="mcp-field">
            <span class="field-label">环境变量</span>
            <span class="field-dim">
              {{ mcpSelected.envKeys.length }} 个（只显示键名）：{{ mcpSelected.envKeys.join('、') }}
            </span>
          </div>
          <div class="mcp-actions">
            <button
              v-if="mcpSelected.transport === 'http'"
              class="ghost-btn"
              :disabled="probeState(mcpSelected) === 'probing'"
              title="对该地址发一次 GET（只读，不发 MCP 请求）"
              @click="probeServer(mcpSelected)"
            >
              {{ probeState(mcpSelected) === 'probing' ? '探活中…' : '探活（GET）' }}
            </button>
            <span class="probe-line" :class="probeState(mcpSelected)">
              {{
                mcpSelected.transport === 'http'
                  ? probeSummary(mcpSelected)
                  : 'stdio 服务由引擎按需启动，Prism 不代启动也不探活'
              }}
            </span>
          </div>
        </div>
      </template>

      <p v-else class="hint">
        {{ mode === 'assets' ? '右侧选择一项资产查看内容' : '左侧选择一项 MCP 服务' }}
      </p>
    </section>
  </div>
</template>

<style scoped src="../styles/components/WorkshopPane.css"></style>
