<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import {
  PERMISSION_OPTIONS,
  WORKSPACE_OPTIONS,
  agentSections,
  agentTags,
  effectiveTools,
  emptyAgentDraft,
  executorBadge,
  executorLabel,
  filterAgents,
  originLabel,
  permissionLabel,
  permissionOption,
  splitList,
  supportsPrivateTools,
  workspaceLabel,
} from '../agents'
import type { AgentOrigin, AgentProfile } from '../types'

const emit = defineEmits<{
  (e: 'error', message: string): void
}>()

const agents = ref<AgentProfile[]>([])
const loading = ref(false)
const loaded = ref(false)
const selectedId = ref<string | null>(null)
/** 新建时的草稿没有 id（用户还没填），单独标记一下。 */
const creating = ref(false)
const draft = ref<AgentProfile | null>(null)
const saving = ref(false)
const notice = ref<string | null>(null)

const originFilter = ref<'all' | AgentOrigin>('all')
const term = ref('')
const tagFilter = ref<string | null>(null)
const tab = ref<'identity' | 'executor' | 'tools' | 'preview'>('identity')

const toolNames = ref<string[]>([])

const visible = computed(() =>
  filterAgents(agents.value, {
    origin: originFilter.value,
    term: term.value,
    tag: tagFilter.value,
  }),
)
const sections = computed(() => agentSections(visible.value))
const tags = computed(() => agentTags(agents.value))

const selected = computed(
  () => agents.value.find(profile => profile.id === selectedId.value) ?? null,
)

/** 编辑中的脏检查：用序列化比较，比逐字段写 watch 稳。 */
const dirty = computed(() => {
  if (!draft.value) return false
  if (creating.value) return true
  return JSON.stringify(draft.value) !== JSON.stringify(selected.value)
})

const idProblem = computed(() => {
  const id = draft.value?.id.trim() ?? ''
  if (!id) return 'id 不能为空'
  if (id.length < 2) return 'id 至少 2 个字符'
  if (!/^[a-z0-9][a-z0-9-_]*$/.test(id)) return '只能用小写字母、数字、- 与 _，且字母数字开头'
  if (id.length > 32) return 'id 最长 32 个字符'
  const taken = agents.value.some(
    profile => profile.id === id && profile.id !== selectedId.value,
  )
  if (taken) return '这个 id 已经存在'
  return null
})
const canSave = computed(
  () => Boolean(draft.value) && !idProblem.value && (draft.value?.name.trim() ?? '') !== '',
)

function report(cause: unknown): void {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

async function load(keepSelection = false): Promise<void> {
  loading.value = true
  try {
    agents.value = await api.agentsList()
    loaded.value = true
    if (!keepSelection || !agents.value.some(item => item.id === selectedId.value)) {
      const first = agents.value[0]
      select(first?.id ?? null)
    }
  } catch (cause) {
    report(cause)
  } finally {
    loading.value = false
  }
}

async function loadTools(): Promise<void> {
  try {
    toolNames.value = (await api.toolList()).filter(tool => tool.enabled).map(tool => tool.name)
  } catch {
    // 工具清单只是预览用的"上限"，取不到就不显示全部可选，不影响编辑。
  }
}

/**
 * 深拷贝一个 Agent。
 *
 * 不用浏览器原生的结构化克隆：`agents` 是 `ref` 里的响应式代理，代理对象过
 * 不了克隆（`DataCloneError`）。这些数据本来就是纯 JSON，走一遍序列化最稳。
 */
function cloneAgent<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function select(id: string | null): void {
  selectedId.value = id
  creating.value = false
  const profile = agents.value.find(item => item.id === id) ?? null
  draft.value = profile ? cloneAgent(profile) : null
  notice.value = null
}

function startCreate(): void {
  creating.value = true
  selectedId.value = null
  draft.value = emptyAgentDraft('')
  tab.value = 'identity'
  notice.value = '新建：id 用短英文（会成为文件名），保存后出现在列表里'
}

function cloneSelected(): void {
  const source = selected.value
  if (!source) return
  creating.value = true
  selectedId.value = null
  const copy = cloneAgent(source)
  copy.id = ''
  copy.name = `${source.name} 副本`
  copy.origin = 'user'
  copy.overridesBuiltin = false
  draft.value = copy
  tab.value = 'identity'
  notice.value = '复制：改一个 id 再保存，内置 preset 不会被改动'
}

async function save(): Promise<void> {
  const profile = draft.value
  if (!profile || !canSave.value || saving.value) return
  saving.value = true
  try {
    const saved = await api.agentSave({ ...profile, id: profile.id.trim() })
    await load(true)
    selectedId.value = saved.id
    const fresh = agents.value.find(item => item.id === saved.id) ?? null
    draft.value = fresh ? cloneAgent(fresh) : null
    creating.value = false
    notice.value = saved.overridesBuiltin
      ? '已覆盖内置 preset：删掉这条自定义即还原'
      : '已保存'
  } catch (cause) {
    report(cause)
  } finally {
    saving.value = false
  }
}

async function remove(): Promise<void> {
  const profile = selected.value
  if (!profile || profile.origin === 'builtin' || saving.value) return
  saving.value = true
  try {
    await api.agentDelete(profile.id)
    selectedId.value = null
    draft.value = null
    await load()
    notice.value = `已删除「${profile.name}」，引用它的计划会在校验时报「缺少 Agent」`
  } catch (cause) {
    report(cause)
  } finally {
    saving.value = false
  }
}

/** 公用工具：`shared === null` 表示"全部"。这里只切换两种状态，避免逐条勾。 */
const sharedAll = computed(() => draft.value?.tools.shared === null)

function toggleSharedAll(): void {
  if (!draft.value) return
  draft.value.tools.shared = draft.value.tools.shared === null ? [...toolNames.value] : null
}

function toggleTool(name: string): void {
  const tools = draft.value?.tools
  if (!tools) return
  const list = tools.shared ?? []
  tools.shared = list.includes(name) ? list.filter(item => item !== name) : [...list, name]
}

function togglePrivate(name: string): void {
  const tools = draft.value?.tools
  if (!tools) return
  tools.private = tools.private.includes(name)
    ? tools.private.filter(item => item !== name)
    : [...tools.private, name]
}

function toggleDeny(name: string): void {
  const tools = draft.value?.tools
  if (!tools) return
  tools.deny = tools.deny.includes(name)
    ? tools.deny.filter(item => item !== name)
    : [...tools.deny, name]
}

const previewTools = computed(() =>
  draft.value ? effectiveTools(draft.value, toolNames.value) : [],
)

watch(term, () => {
  // 过滤后当前选中项可能不在列表里：不强制改选，避免编辑中的草稿被切掉。
})

onMounted(() => {
  void load()
  void loadTools()
})

defineExpose({ reload: () => load(true) })
</script>

<template>
  <div class="agents-pane">
    <aside class="pane nav">
      <div class="pane-head">
        <div class="pane-title">Agents</div>
        <span class="pane-count">{{ agents.length }}</span>
        <button class="ghost-btn" :disabled="loading" title="重新读取 Agent 注册表" @click="load(true)">
          刷新
        </button>
      </div>
      <div class="pane-body">
        <p v-if="loading && !loaded" class="hint">正在读取 Agent 注册表…</p>
        <template v-else>
          <input v-model="term" class="filter" placeholder="过滤名称 / id / 标签…" spellcheck="false" />
          <div class="seg-row">
            <button
              v-for="option in (['all', 'builtin', 'user'] as const)"
              :key="option"
              class="seg"
              :class="{ active: originFilter === option }"
              @click="originFilter = option"
            >
              {{ option === 'all' ? '全部' : option === 'builtin' ? '内置' : '自定义' }}
            </button>
          </div>
          <div v-if="tags.length" class="tag-filter">
            <button
              v-for="tag in tags"
              :key="tag"
              class="tag"
              :class="{ active: tagFilter === tag }"
              @click="tagFilter = tagFilter === tag ? null : tag"
            >
              {{ tag }}
            </button>
          </div>

          <p v-if="!visible.length" class="hint">
            {{ agents.length ? '没有匹配项' : '注册表是空的' }}
          </p>

          <section v-if="sections.builtin.length" class="nav-section">
            <div class="nav-engine">内置 <span class="count">{{ sections.builtin.length }}</span></div>
            <button
              v-for="profile in sections.builtin"
              :key="profile.id"
              class="agent-row"
              :class="{ active: profile.id === selectedId && !creating }"
              @click="select(profile.id)"
            >
              <span class="agent-avatar" :class="profile.executor">{{ profile.name.slice(0, 1) }}</span>
              <span class="agent-row-copy">
                <span class="agent-row-name">{{ profile.name }}</span>
                <span class="agent-row-sub">{{ profile.tags.join(' · ') || profile.id }}</span>
              </span>
              <span class="dot" :class="profile.permission" />
            </button>
          </section>

          <section v-if="sections.user.length" class="nav-section">
            <div class="nav-engine">自定义 <span class="count">{{ sections.user.length }}</span></div>
            <button
              v-for="profile in sections.user"
              :key="profile.id"
              class="agent-row"
              :class="{ active: profile.id === selectedId && !creating }"
              @click="select(profile.id)"
            >
              <span class="agent-avatar" :class="profile.executor">{{ profile.name.slice(0, 1) }}</span>
              <span class="agent-row-copy">
                <span class="agent-row-name">{{ profile.name }}</span>
                <span class="agent-row-sub">
                  {{ profile.overridesBuiltin ? '覆盖内置' : '自定义' }}
                </span>
              </span>
              <span class="dot" :class="profile.permission" />
            </button>
          </section>
        </template>
      </div>
      <div class="pane-foot">
        <button class="primary-btn" @click="startCreate">＋ 新建 Agent</button>
      </div>
    </aside>

    <section class="pane grid">
      <div class="pane-head">
        <div class="pane-title">{{ creating ? '新建 Agent' : '全部 Agent' }}</div>
        <span class="pane-count">{{ visible.length }}</span>
        <span class="pane-note">每个 Agent 声明自己的执行器、工具策略与权限档</span>
      </div>
      <div class="pane-body grid-body">
        <p v-if="!visible.length && loaded" class="hint">
          还没有 Agent。点左下「新建 Agent」，或从内置项「复制为」开始。
        </p>
        <div class="card-grid">
          <button
            v-for="profile in visible"
            :key="profile.id"
            class="agent-card"
            :class="{ active: profile.id === selectedId && !creating }"
            @click="select(profile.id)"
          >
            <span class="card-top">
              <span class="card-avatar" :class="profile.executor">{{ profile.name.slice(0, 1) }}</span>
              <span class="card-copy">
                <span class="card-name-row">
                  <span class="card-name">{{ profile.name }}</span>
                  <span class="origin-badge" :class="profile.origin">
                    {{ originLabel(profile) }}
                  </span>
                </span>
                <span class="card-id">{{ profile.id }}</span>
              </span>
            </span>
            <span class="card-desc">{{ profile.description || '（还没有简介）' }}</span>
            <span class="card-tags">
              <span v-for="tag in profile.tags" :key="tag" class="tag">{{ tag }}</span>
            </span>
            <span class="card-foot">
              <span class="eng" :class="profile.executor">{{ executorBadge(profile.executor) }}</span>
              <span class="card-meta">
                {{ profile.tools.shared === null ? '全部公用' : `${profile.tools.shared.length} 公用` }}
                <template v-if="profile.tools.private.length">
                  · {{ profile.tools.private.length }} 私有
                </template>
              </span>
              <span class="spacer" />
              <span class="perm" :class="profile.permission">{{ permissionLabel(profile.permission) }}</span>
            </span>
          </button>
        </div>
      </div>
    </section>

    <aside class="pane insp">
      <template v-if="draft">
        <div class="pane-head">
          <div class="pane-title">
            {{ creating ? '新建' : '详情' }}
            <span v-if="dirty" class="dirty-dot" title="有未保存的改动" />
          </div>
          <button class="ghost-btn" :disabled="!dirty" title="放弃改动" @click="select(selectedId)">
            还原
          </button>
        </div>

        <div class="insp-tabs">
          <button
            v-for="item in ([
              ['identity', '身份'],
              ['executor', '执行器'],
              ['tools', '工具'],
              ['preview', '预览'],
            ] as const)"
            :key="item[0]"
            class="insp-tab"
            :class="{ active: tab === item[0] }"
            @click="tab = item[0]"
          >
            {{ item[1] }}
          </button>
        </div>

        <div class="pane-body insp-body">
          <p v-if="notice" class="notice">{{ notice }}</p>

          <template v-if="tab === 'identity'">
            <label class="field">
              <span class="field-label">id <em>文件名，保存后不易改</em></span>
              <input v-model="draft.id" :disabled="!creating" spellcheck="false" />
              <span v-if="idProblem" class="field-error">{{ idProblem }}</span>
            </label>
            <label class="field">
              <span class="field-label">名称</span>
              <input v-model="draft.name" placeholder="前端工程师" />
              <span v-if="!draft.name.trim()" class="field-error">名称不能为空</span>
            </label>
            <label class="field">
              <span class="field-label">简介</span>
              <textarea v-model="draft.description" rows="3" placeholder="一句话说清它负责什么" />
            </label>
            <label class="field">
              <span class="field-label">标签 <em>逗号分隔，用于筛选</em></span>
              <input
                :value="draft.tags.join(', ')"
                placeholder="前端, 实现"
                @input="draft.tags = splitList(($event.target as HTMLInputElement).value)"
              />
            </label>
            <div v-if="!creating && draft.origin === 'builtin'" class="tip">
              这是内置 preset。保存会写入 <code>user/{{ draft.id }}.json</code> 覆盖它；
              删掉这条自定义即还原。
            </div>
          </template>

          <template v-else-if="tab === 'executor'">
            <label class="field">
              <span class="field-label">执行器</span>
              <select v-model="draft.executor">
                <option value="native">{{ executorLabel('native') }}</option>
                <option value="dsh">{{ executorLabel('dsh') }}</option>
                <option value="codex">{{ executorLabel('codex') }}</option>
                <option value="opencode">{{ executorLabel('opencode') }}</option>
                <option value="claude">{{ executorLabel('claude') }}</option>
              </select>
            </label>
            <div v-if="!supportsPrivateTools(draft.executor)" class="tip warn">
              外部引擎自己管工具：Prism 只能通过会话参数影响它，私有工具与系统提示
              不会像原生引擎那样强制生效。
            </div>
            <label class="field">
              <span class="field-label">模型 <em>留空跟随全局默认</em></span>
              <input v-model="draft.model" placeholder="deepseek-chat" spellcheck="false" />
            </label>
            <label class="field">
              <span class="field-label">系统提示</span>
              <textarea v-model="draft.systemPrompt" rows="6" placeholder="你负责…先读再改…" />
            </label>
            <label class="field">
              <span class="field-label">权限档</span>
              <div class="perm-list">
                <button
                  v-for="option in PERMISSION_OPTIONS"
                  :key="option.id"
                  class="perm-option"
                  :class="{ active: draft.permission === option.id, risky: option.risky }"
                  @click="draft.permission = option.id"
                >
                  <span class="perm-option-head">
                    {{ option.label }}
                    <span v-if="option.risky" class="flag">谨慎</span>
                  </span>
                  <span class="perm-option-desc">{{ option.description }}</span>
                </button>
              </div>
            </label>
            <label class="field">
              <span class="field-label">默认工作区</span>
              <div class="perm-list">
                <button
                  v-for="option in WORKSPACE_OPTIONS"
                  :key="option.id"
                  class="perm-option"
                  :class="{ active: draft.workspace === option.id }"
                  @click="draft.workspace = option.id"
                >
                  <span class="perm-option-head">{{ option.label }}</span>
                  <span class="perm-option-desc">{{ option.description }}</span>
                </button>
              </div>
            </label>
            <label class="field checkbox">
              <input v-model="draft.gateDefault" type="checkbox" />
              <span>该 Agent 的节点默认要人工确认</span>
            </label>
          </template>

          <template v-else-if="tab === 'tools'">
            <div class="field">
              <span class="field-label">
                公用工具
                <em>{{ sharedAll ? '全部可用' : `${draft.tools.shared?.length ?? 0} 个` }}</em>
                <button class="link-btn" @click="toggleSharedAll">
                  {{ sharedAll ? '改成逐条选' : '改成全部' }}
                </button>
              </span>
              <p v-if="sharedAll" class="hint small">
                内置与外部公用工具全部可见。想收紧就改成逐条选。
              </p>
              <div v-else class="chip-wrap">
                <button
                  v-for="name in toolNames"
                  :key="name"
                  class="chip"
                  :class="{ active: draft.tools.shared?.includes(name) }"
                  @click="toggleTool(name)"
                >
                  {{ name }}
                </button>
                <p v-if="!toolNames.length" class="hint small">工具清单读取失败或为空</p>
              </div>
            </div>

            <label class="field">
              <span class="field-label">私有工具 <em>只对这个 Agent 可见</em></span>
              <input
                :value="draft.tools.private.join(', ')"
                placeholder="figma__export, sql__query"
                spellcheck="false"
                @input="draft.tools.private = splitList(($event.target as HTMLInputElement).value)"
              />
              <div class="chip-wrap">
                <button
                  v-for="name in draft.tools.private"
                  :key="name"
                  class="chip active private"
                  @click="togglePrivate(name)"
                >
                  {{ name }}
                </button>
              </div>
            </label>

            <div class="field">
              <span class="field-label">明确拒绝 <em>优先于允许</em></span>
              <div class="chip-wrap">
                <button
                  v-for="name in toolNames"
                  :key="name"
                  class="chip"
                  :class="{ active: draft.tools.deny.includes(name) }"
                  @click="toggleDeny(name)"
                >
                  {{ name }}
                </button>
              </div>
            </div>
          </template>

          <template v-else>
            <div class="field">
              <span class="field-label">
                模型会看到 <em>{{ previewTools.length }} 个工具</em>
              </span>
              <div class="chip-wrap">
                <span v-for="name in previewTools" :key="name" class="chip static">{{ name }}</span>
                <p v-if="!previewTools.length" class="hint small">一个都不给：这个 Agent 只能空手回答</p>
              </div>
            </div>
            <div class="field">
              <span class="field-label">解析顺序</span>
              <p class="hint small">
                公用（或全部）→ 加私有 → 去掉拒绝 → 去重。权限档过滤在后端完成，
                所以这里是上限而不是最终清单。
              </p>
            </div>
            <div class="field">
              <span class="field-label">速览</span>
              <div class="kv"><span>执行器</span><b>{{ executorLabel(draft.executor) }}</b></div>
              <div class="kv"><span>权限</span><b>{{ permissionLabel(draft.permission) }}</b></div>
              <div class="kv"><span>工作区</span><b>{{ workspaceLabel(draft.workspace) }}</b></div>
              <div class="kv"><span>来源</span><b>{{ originLabel(draft) }}</b></div>
              <div class="kv">
                <span>需要确认</span>
                <b>{{ draft.gateDefault || permissionOption(draft.permission).risky ? '是' : '否' }}</b>
              </div>
            </div>
          </template>
        </div>

        <div class="insp-foot">
          <button class="primary-btn" :disabled="!canSave || saving || !dirty" @click="save">
            {{ saving ? '保存中…' : draft.origin === 'builtin' && !creating ? '保存（覆盖内置）' : '保存' }}
          </button>
          <button v-if="!creating" class="ghost-btn" @click="cloneSelected">复制为新</button>
          <button
            v-if="!creating && draft.origin === 'user'"
            class="danger-btn"
            :disabled="saving"
            title="删掉自定义文件；若是覆盖内置，删完就还原"
            @click="remove"
          >
            删除
          </button>
        </div>
      </template>

      <p v-else class="hint">左侧选一个 Agent，或在左下新建一个</p>
    </aside>
  </div>
</template>

<style scoped src="../styles/components/AgentsPane.css"></style>
