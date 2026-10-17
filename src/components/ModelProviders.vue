<script setup lang="ts">
/**
 * 模型管理面板：原生引擎的「服务 → 模型」配置。
 *
 * 布局参照 DeepSeek Harness 的 Settings → Models：一个服务一张卡，卡里是
 * 勾选过的模型；新增/编辑走「名称 + Base URL + 密钥 → 拉取模型 → 勾选」，
 * 全局只有一个默认模型。密钥只写不读，界面只拿到末四位。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { api } from '../api'
import type { NativeConfigView, NativeModel, NativeProvider } from '../types'

const emit = defineEmits<{
  (e: 'changed', config: NativeConfigView): void
}>()

const PRESETS = [
  { label: 'DeepSeek', name: 'DeepSeek', baseUrl: 'https://api.deepseek.com/v1' },
  { label: 'OpenAI', name: 'OpenAI', baseUrl: 'https://api.openai.com/v1' },
  { label: 'Ollama', name: 'Ollama', baseUrl: 'http://127.0.0.1:11434/v1' },
] as const

interface Draft {
  id: string | null
  label: string
  baseUrl: string
  apiKey: string
  clearKey: boolean
  hasKey: boolean
  keyHint: string
  models: string[]
}

const config = ref<NativeConfigView | null>(null)
const loading = ref(true)
const error = ref<string | null>(null)
const notice = ref<string | null>(null)
const busy = ref(false)
const testing = ref(false)
const fetching = ref(false)

const draft = ref<Draft | null>(null)
const candidates = ref<NativeModel[]>([])
const search = ref('')
const manual = ref('')
const confirmId = ref<string | null>(null)
const maxTurns = ref(12)
let confirmTimer = 0
let noticeTimer = 0

const activeLabel = computed(() => {
  const active = config.value?.active
  if (!active) return ''
  const provider = config.value?.providers.find(item => item.id === active.provider)
  return provider ? `${provider.label} / ${active.model}` : active.model
})

const visibleCandidates = computed(() => {
  const keyword = search.value.trim().toLowerCase()
  if (!keyword) return candidates.value
  return candidates.value.filter(
    item =>
      item.id.toLowerCase().includes(keyword) ||
      item.label.toLowerCase().includes(keyword),
  )
})

onMounted(() => {
  void load()
})

onBeforeUnmount(() => {
  window.clearTimeout(confirmTimer)
  window.clearTimeout(noticeTimer)
})

function flash(text: string) {
  notice.value = text
  error.value = null
  window.clearTimeout(noticeTimer)
  noticeTimer = window.setTimeout(() => {
    notice.value = null
  }, 3600)
}

function fail(cause: unknown) {
  notice.value = null
  error.value = cause instanceof Error ? cause.message : String(cause)
}

function adopt(next: NativeConfigView) {
  config.value = next
  maxTurns.value = next.maxTurns
  emit('changed', next)
}

async function load() {
  loading.value = true
  try {
    adopt(await api.nativeConfig())
  } catch (cause) {
    fail(cause)
  } finally {
    loading.value = false
  }
}

function protocolLabel(protocol: string): string {
  return protocol === 'openai' ? 'OpenAI 兼容' : protocol
}

function startAdd() {
  draft.value = {
    id: null,
    label: '',
    baseUrl: '',
    apiKey: '',
    clearKey: false,
    hasKey: false,
    keyHint: '',
    models: [],
  }
  candidates.value = []
  search.value = ''
  manual.value = ''
  error.value = null
}

function startEdit(provider: NativeProvider) {
  draft.value = {
    id: provider.id,
    label: provider.label,
    baseUrl: provider.baseUrl,
    apiKey: '',
    clearKey: false,
    hasKey: provider.hasKey,
    keyHint: provider.keyHint,
    models: provider.models.map(model => model.id),
  }
  // 已保存的模型直接铺成候选，省掉一次「拉取」才能看全。
  candidates.value = provider.models.map(model => ({ ...model }))
  search.value = ''
  manual.value = ''
  error.value = null
}

function applyPreset(preset: (typeof PRESETS)[number]) {
  if (!draft.value) return
  draft.value.label = draft.value.label.trim() || preset.name
  draft.value.baseUrl = preset.baseUrl
}

function cancel() {
  draft.value = null
  candidates.value = []
  error.value = null
}

function toggleModel(id: string) {
  const current = draft.value
  if (!current) return
  current.models = current.models.includes(id)
    ? current.models.filter(item => item !== id)
    : [...current.models, id]
}

function removeModel(id: string) {
  const current = draft.value
  if (!current) return
  current.models = current.models.filter(item => item !== id)
}

function addManual() {
  const current = draft.value
  const id = manual.value.trim()
  if (!current || !id) return
  if (!current.models.includes(id)) current.models = [...current.models, id]
  manual.value = ''
}

function selectAllVisible() {
  const current = draft.value
  if (!current) return
  const merged = new Set(current.models)
  for (const item of visibleCandidates.value) merged.add(item.id)
  current.models = [...merged]
}

function invertVisible() {
  const current = draft.value
  if (!current) return
  const visible = new Set(visibleCandidates.value.map(item => item.id))
  const kept = current.models.filter(id => !visible.has(id))
  const added = [...visible].filter(id => !current.models.includes(id))
  current.models = [...kept, ...added]
}

function clearVisible() {
  const current = draft.value
  if (!current) return
  const visible = new Set(visibleCandidates.value.map(item => item.id))
  current.models = current.models.filter(id => !visible.has(id))
}

async function fetchModels() {
  const current = draft.value
  if (!current) return
  if (!current.baseUrl.trim()) {
    error.value = '先填 Base URL'
    return
  }
  fetching.value = true
  error.value = null
  try {
    const list = await api.nativeModelsFetch({
      providerId: current.id,
      baseUrl: current.baseUrl.trim(),
      apiKey: current.apiKey.trim() || null,
    })
    candidates.value = list
    search.value = ''
    flash(`拉到 ${list.length} 个模型`)
  } catch (cause) {
    fail(cause)
  } finally {
    fetching.value = false
  }
}

async function save() {
  const current = draft.value
  if (!current) return
  if (!current.baseUrl.trim()) {
    error.value = 'Base URL 不能为空'
    return
  }
  if (!current.models.length) {
    error.value = '至少勾选一个模型'
    return
  }
  busy.value = true
  error.value = null
  try {
    const next = await api.nativeProviderSave({
      id: current.id,
      label: current.label.trim(),
      baseUrl: current.baseUrl.trim(),
      apiKey: current.apiKey.trim() || undefined,
      clearKey: current.clearKey,
      models: current.models.map(id => ({ id })),
    })
    const name = current.label.trim() || current.baseUrl
    draft.value = null
    candidates.value = []
    adopt(next)
    flash(`已保存 ${name}`)
  } catch (cause) {
    fail(cause)
  } finally {
    busy.value = false
  }
}

async function select(provider: NativeProvider, model: NativeModel) {
  if (model.active || busy.value) return
  busy.value = true
  error.value = null
  try {
    adopt(await api.nativeModelSelect(provider.id, model.id))
    flash(`默认模型 → ${model.id}`)
  } catch (cause) {
    fail(cause)
  } finally {
    busy.value = false
  }
}

function askRemove(provider: NativeProvider) {
  if (confirmId.value !== provider.id) {
    confirmId.value = provider.id
    window.clearTimeout(confirmTimer)
    confirmTimer = window.setTimeout(() => {
      confirmId.value = null
    }, 4000)
    return
  }
  confirmId.value = null
  void remove(provider)
}

async function remove(provider: NativeProvider) {
  busy.value = true
  error.value = null
  try {
    adopt(await api.nativeProviderDelete(provider.id))
    if (draft.value?.id === provider.id) cancel()
    flash(`已删除 ${provider.label}`)
  } catch (cause) {
    fail(cause)
  } finally {
    busy.value = false
  }
}

async function saveLimits() {
  const turns = Math.max(1, Math.min(40, Math.round(maxTurns.value || 12)))
  maxTurns.value = turns
  try {
    adopt(await api.nativeLimitsSet({ maxTurns: turns }))
    flash(`单轮上限 → ${turns} 次`)
  } catch (cause) {
    fail(cause)
  }
}

async function testActive() {
  const active = config.value?.active
  if (!active) return
  testing.value = true
  error.value = null
  try {
    flash(await api.nativeProbe(active.provider, active.model))
  } catch (cause) {
    fail(cause)
  } finally {
    testing.value = false
  }
}
</script>

<template>
  <div class="mp">
    <p v-if="error" class="mp-banner mp-error" role="alert">{{ error }}</p>
    <p v-else-if="notice" class="mp-banner mp-ok">{{ notice }}</p>

    <div class="mp-card">
      <header class="mp-card-head">
        <span class="mp-card-title">默认与运行</span>
        <span class="mp-card-hint">会话里没单独切模型时用它。</span>
      </header>
      <div class="mp-row">
        <span class="mp-row-key">默认模型</span>
        <span v-if="activeLabel" class="mp-row-val mono">{{ activeLabel }}</span>
        <span v-else class="mp-row-val is-faint">还没选中</span>
        <span class="mp-spacer" />
        <button
          class="mp-btn"
          :disabled="!config?.ready || testing"
          @click="testActive"
        >
          {{ testing ? '测试中…' : '测试连接' }}
        </button>
      </div>
      <div class="mp-row">
        <span class="mp-row-key">单轮最多</span>
        <input
          v-model.number="maxTurns"
          class="mp-num mono"
          type="number"
          min="1"
          max="40"
          @change="saveLimits"
        />
        <span class="mp-row-suffix">次工具往返</span>
      </div>
    </div>

    <div class="mp-card">
      <header class="mp-card-head">
        <span class="mp-card-title">模型服务</span>
        <span class="mp-card-hint">每个服务一份地址与密钥，勾选的模型会进会话的模型下拉。</span>
        <span class="mp-spacer" />
        <button v-if="!draft" class="mp-btn mp-btn-primary" @click="startAdd">
          ＋ 添加服务
        </button>
      </header>

      <form v-if="draft" class="mp-editor" @submit.prevent="save">
        <div class="mp-presets">
          <span class="mp-label">快速填入</span>
          <button
            v-for="preset in PRESETS"
            :key="preset.label"
            type="button"
            class="mp-preset"
            :class="{ on: draft.baseUrl === preset.baseUrl }"
            @click="applyPreset(preset)"
          >
            {{ preset.label }}
          </button>
          <span class="mp-spacer" />
          <span v-if="draft.id" class="mp-id-chip mono">id: {{ draft.id }}</span>
        </div>

        <div class="mp-grid">
          <label class="mp-field">
            <span class="mp-label">名称</span>
            <input v-model="draft.label" class="mp-input" placeholder="DeepSeek" />
          </label>
          <label class="mp-field">
            <span class="mp-label">Base URL</span>
            <input
              v-model="draft.baseUrl"
              class="mp-input mono"
              placeholder="https://api.deepseek.com/v1"
            />
          </label>
        </div>

        <label class="mp-field">
          <span class="mp-label">API Key</span>
          <input
            v-model="draft.apiKey"
            class="mp-input mono"
            type="password"
            autocomplete="off"
            :placeholder="
              draft.hasKey ? `已保存 ····${draft.keyHint}（留空沿用）` : 'sk-…'
            "
          />
        </label>

        <div class="mp-field">
          <div class="mp-models-head">
            <span class="mp-label">模型</span>
            <span class="mp-count">{{ draft.models.length }} 个已选</span>
            <span class="mp-spacer" />
            <button type="button" class="mp-btn" :disabled="fetching" @click="fetchModels">
              {{ fetching ? '拉取中…' : '拉取模型' }}
            </button>
          </div>

          <div v-if="candidates.length" class="mp-picker">
            <div class="mp-picker-bar">
              <input v-model="search" class="mp-input mono mp-search" placeholder="搜索模型 id" />
              <button type="button" class="mp-link" @click="selectAllVisible">全选</button>
              <button type="button" class="mp-link" @click="invertVisible">反选</button>
              <button type="button" class="mp-link" @click="clearVisible">清空</button>
              <span class="mp-spacer" />
              <span class="mp-count">{{ visibleCandidates.length }} / {{ candidates.length }}</span>
            </div>
            <div class="mp-cand-list">
              <label
                v-for="model in visibleCandidates"
                :key="model.id"
                class="mp-cand"
                :class="{ on: draft.models.includes(model.id) }"
              >
                <input
                  type="checkbox"
                  :checked="draft.models.includes(model.id)"
                  @change="toggleModel(model.id)"
                />
                <span class="mp-cand-id mono">{{ model.id }}</span>
                <span v-if="model.label !== model.id" class="mp-cand-label">
                  {{ model.label }}
                </span>
              </label>
              <p v-if="!visibleCandidates.length" class="mp-empty">没有匹配的模型</p>
            </div>
          </div>

          <div v-if="draft.models.length" class="mp-chips">
            <span v-for="id in draft.models" :key="id" class="mp-chip">
              <span class="mono">{{ id }}</span>
              <button type="button" class="mp-chip-x" title="移除" @click="removeModel(id)">
                ×
              </button>
            </span>
          </div>

          <div class="mp-manual">
            <input
              v-model="manual"
              class="mp-input mono"
              placeholder="也可以手填模型 id 后回车"
              @keydown.enter.prevent="addManual"
            />
            <button type="button" class="mp-btn" :disabled="!manual.trim()" @click="addManual">
              加入
            </button>
          </div>
        </div>

        <footer class="mp-editor-foot">
          <span class="mp-hint">
            密钥只存在本机 <code>~/.prism/native/config.json</code>（0600），界面只显示末四位。
          </span>
          <span class="mp-spacer" />
          <button type="button" class="mp-btn" :disabled="busy" @click="cancel">取消</button>
          <button
            type="submit"
            class="mp-btn mp-btn-primary"
            :disabled="busy || !draft.baseUrl.trim()"
          >
            {{ busy ? '保存中…' : '保存服务' }}
          </button>
        </footer>
      </form>

      <div v-if="loading" class="mp-empty mp-loading">读取配置…</div>
      <p v-else-if="!config?.providers.length && !draft" class="mp-empty">
        还没有模型服务。点右上角「添加服务」，填地址和密钥后拉取模型。
      </p>

      <article
        v-for="provider in config?.providers ?? []"
        :key="provider.id"
        class="mp-provider"
        :class="{ 'is-active': provider.active }"
      >
        <header class="mp-provider-head">
          <span class="mp-dot" :class="{ ok: provider.ready }" />
          <span class="mp-provider-name">{{ provider.label }}</span>
          <span class="mp-tag">{{ protocolLabel(provider.protocol) }}</span>
          <span v-if="provider.active" class="mp-tag mp-tag-accent">当前</span>
          <span class="mp-spacer" />
          <code class="mp-provider-url" :title="provider.baseUrl">{{ provider.baseUrl }}</code>
          <button class="mp-icon" title="编辑服务" @click="startEdit(provider)">编辑</button>
          <button
            class="mp-icon mp-icon-danger"
            :class="{ armed: confirmId === provider.id }"
            @click="askRemove(provider)"
          >
            {{ confirmId === provider.id ? '确认删除' : '删除' }}
          </button>
        </header>

        <div class="mp-provider-models">
          <button
            v-for="model in provider.models"
            :key="model.id"
            class="mp-model"
            :class="{ on: model.active }"
            :disabled="!provider.ready || busy"
            @click="select(provider, model)"
          >
            <span class="mp-radio" />
            <span class="mp-model-id mono">{{ model.id }}</span>
            <span v-if="model.label !== model.id" class="mp-model-label">
              {{ model.label }}
            </span>
          </button>
          <span v-if="!provider.models.length" class="mp-empty mp-empty-inline">
            还没勾模型，点「编辑」拉取
          </span>
        </div>

        <footer class="mp-provider-foot">
          <span>{{ provider.hasKey ? `密钥 ····${provider.keyHint}` : '未填密钥' }}</span>
          <span class="mp-sep">·</span>
          <span>{{ provider.models.length }} 个模型</span>
          <span class="mp-sep">·</span>
          <span class="mono">{{ provider.id }}</span>
        </footer>
      </article>
    </div>
  </div>
</template>

<style scoped src="../styles/components/ModelProviders.css"></style>
