<script setup lang="ts">
/**
 * 新建会话组合器（M20）：引擎（OpenCode / Codex / Claude Code）× 项目 ×
 * 模型 × 推理档位 × 首条指令（@ 文件 / / 命令 / ＋上下文 / 附件 chips）。
 * 提交 = 建会话（必要时空会话）+ 有内容则立即发送，随后通知父级开列。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { applyCompletion, detectTrigger, fileLabel, modelKeyOf, modelLabel } from '../composer'
import { ASSET_ENGINE_LABELS } from '../workshop'
import type {
  EngineCatalog,
  EngineCommand,
  EngineModel,
  FileHit,
  GitSnapshot,
  ProjectItem,
  SendOptions,
} from '../types'
import type { Trigger } from '../composer'
import SelectMenu from './SelectMenu.vue'

const props = defineProps<{
  projects: ProjectItem[]
}>()

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'created', sessionId: string): void
  (e: 'error', message: string): void
}>()

const ENGINES = ['opencode', 'codex', 'claude'] as const
type EngineId = (typeof ENGINES)[number]

const engine = ref<EngineId>('opencode')
const projectId = ref<string>(props.projects[0]?.id ?? '')
const catalog = ref<EngineCatalog | null>(null)
const catalogLoading = ref(false)
const modelKey = ref('') // '' = 引擎默认；否则 `${provider}::${id}` 或 codex 的 id
const variant = ref('') // '' = 引擎默认
const draft = ref('')
const attachments = ref<FileHit[]>([])
const saving = ref(false)
const textarea = ref<HTMLTextAreaElement | null>(null)

interface Popup {
  kind: 'file' | 'command'
  items: (FileHit | EngineCommand)[]
  active: number
  trigger: Trigger
}
const popup = ref<Popup | null>(null)
let popupTimer: number | null = null

const project = computed(() => props.projects.find(item => item.id === projectId.value) ?? null)
const models = computed<EngineModel[]>(() => catalog.value?.models ?? [])
const commands = computed<EngineCommand[]>(() => catalog.value?.commands ?? [])
const selectedModel = computed<EngineModel | null>(() => {
  if (!modelKey.value) return null
  return models.value.find(model => modelKeyOf(model) === modelKey.value) ?? null
})
const variants = computed(() => selectedModel.value?.variants ?? [])
const hasContent = computed(() => !!draft.value.trim() || attachments.value.length > 0)

/** 三个下拉的选项（含空值 = 引擎默认）。 */
const projectOptions = computed(() =>
  props.projects.map(item => ({ value: item.id, label: item.name || item.worktree })),
)
const modelOptions = computed(() => [
  { value: '', label: '（引擎默认）' },
  ...models.value.map(model => ({ value: modelKeyOf(model), label: modelLabel(model) })),
])
const variantOptions = computed(() => [
  { value: '', label: '（引擎默认）' },
  ...variants.value.map(item => ({ value: item, label: item })),
])

// ---- 目录加载 ----

async function loadCatalog() {
  const target = project.value
  if (!target) {
    catalog.value = null
    return
  }
  catalogLoading.value = true
  modelKey.value = ''
  variant.value = ''
  try {
    const loaded = await api.engineCatalog(engine.value, target.worktree)
    catalog.value = loaded
  } catch (cause) {
    catalog.value = null
    emit('error', cause instanceof Error ? cause.message : String(cause))
  } finally {
    catalogLoading.value = false
  }
}

watch([engine, projectId], () => {
  void loadCatalog()
})

// ---- @ 文件 / / 命令补全 ----

function closePopup() {
  popup.value = null
  if (popupTimer !== null) {
    window.clearTimeout(popupTimer)
    popupTimer = null
  }
}

function caret(): number {
  const el = textarea.value
  return el?.selectionStart ?? draft.value.length
}

function onInput() {
  const trigger = detectTrigger(draft.value, caret())
  if (!trigger) {
    closePopup()
    return
  }
  if (trigger.kind === 'command') {
    const term = trigger.term.toLowerCase()
    const items = commands.value.filter(command => command.name.toLowerCase().includes(term))
    popup.value = items.length ? { kind: 'command', items, active: 0, trigger } : null
    return
  }
  // 文件：防抖后向项目目录要索引
  if (popupTimer !== null) window.clearTimeout(popupTimer)
  const target = project.value
  if (!target) return
  popupTimer = window.setTimeout(async () => {
    popupTimer = null
    try {
      const hits = await api.projectFiles(target.worktree, trigger.term, 20)
      // 拿结果时触发词可能已被改掉：再核对一次
      const current = detectTrigger(draft.value, caret())
      if (!current || current.kind !== 'file' || current.start !== trigger.start) return
      popup.value = hits.length ? { kind: 'file', items: hits, active: 0, trigger: current } : null
    } catch {
      popup.value = null
    }
  }, 180)
}

async function pickPopup(index: number) {
  const current = popup.value
  if (!current || !current.items[index]) return
  const item = current.items[index]
  if (current.kind === 'file') {
    const file = item as FileHit
    if (!attachments.value.some(entry => entry.path === file.path)) {
      attachments.value = [...attachments.value, file]
    }
    await insertText(`@${file.path}`)
  } else {
    const command = item as EngineCommand
    await insertText(`/${command.name}`)
  }
  closePopup()
}

async function insertText(replacement: string) {
  const current = popup.value
  if (!current) return
  const next = applyCompletion(draft.value, current.trigger, replacement, caret())
  draft.value = next.text
  await nextTick()
  const el = textarea.value
  if (el) {
    el.focus()
    el.setSelectionRange(next.caret, next.caret)
  }
}

function removeAttachment(file: FileHit) {
  attachments.value = attachments.value.filter(entry => entry.path !== file.path)
  const token = `@${file.path}`
  draft.value = draft.value.split(token).join('').replace(/ {2,}/g, ' ')
}

function onKeydown(event: KeyboardEvent) {
  if (popup.value && popup.value.items.length) {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      popup.value.active = (popup.value.active + 1) % popup.value.items.length
      return
    }
    if (event.key === 'ArrowUp') {
      event.preventDefault()
      popup.value.active =
        (popup.value.active - 1 + popup.value.items.length) % popup.value.items.length
      return
    }
    if (event.key === 'Enter' || event.key === 'Tab') {
      event.preventDefault()
      void pickPopup(popup.value.active)
      return
    }
    if (event.key === 'Escape') {
      event.preventDefault()
      closePopup()
      return
    }
  }
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault()
    void submit()
  }
}

// ---- ＋上下文 ----

function snapshotToText(snapshot: GitSnapshot): string {
  const lines: string[] = ['[Git 上下文]']
  if (snapshot.branch) lines.push(`分支：${snapshot.branch}`)
  if (snapshot.commits.length) {
    lines.push('最近提交：')
    lines.push(...snapshot.commits.map(line => `- ${line}`))
  }
  if (snapshot.dirty.length) {
    lines.push('未提交变更：')
    lines.push(...snapshot.dirty.map(line => `- ${line}`))
  } else {
    lines.push('工作区干净。')
  }
  return lines.join('\n')
}

async function insertContext() {
  const target = project.value
  if (!target) return
  try {
    const snapshot = await api.gitSnapshot(target.worktree)
    const block = snapshotToText(snapshot)
    draft.value = draft.value.trim() ? `${draft.value.trimEnd()}\n\n${block}\n` : `${block}\n`
    await nextTick()
    textarea.value?.focus()
  } catch (cause) {
    emit('error', cause instanceof Error ? cause.message : String(cause))
  }
}

// ---- 提交 ----

async function submit() {
  const target = project.value
  if (!target || saving.value) return
  if (catalog.value && !catalog.value.available) {
    emit('error', catalog.value.reason ?? '该引擎暂不可用')
    return
  }
  saving.value = true
  try {
    const created = await api.sessionCreate(target.worktree, engine.value)
    const text = draft.value.trim()
    const files = attachments.value.map(file => ({
      uri: `${target.worktree.replace(/\/+$/, '')}/${file.path}`,
      name: fileLabel(file.path),
    }))
    if (text || files.length) {
      // 会话行写入引擎侧可能有延迟（M17 赛马同款竞态）：稍等再发。
      await new Promise(resolve => window.setTimeout(resolve, 250))
      const options: SendOptions = { files }
      const model = selectedModel.value
      if (model) {
        options.model = model.id
        options.provider = model.provider
        if (variant.value && variant.value !== 'default') options.variant = variant.value
      }
      await api.sessionSend(created.sessionId, text, options)
    }
    emit('created', created.sessionId)
  } catch (cause) {
    emit('error', cause instanceof Error ? cause.message : String(cause))
  } finally {
    saving.value = false
  }
}

function onKeydownWindow(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    if (popup.value) {
      closePopup()
      return
    }
    emit('close')
  }
}

onMounted(() => {
  window.addEventListener('keydown', onKeydownWindow)
  void loadCatalog()
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydownWindow)
  closePopup()
})
</script>

<template>
  <div class="dialog-overlay composer-overlay" @click.self="emit('close')">
    <div class="dialog-panel composer-panel" role="dialog" aria-label="新建会话">
      <header class="dialog-head">
        <span class="dialog-title">新建会话</span>
        <button class="close-btn" title="关闭（Esc）" @click="emit('close')">✕</button>
      </header>

      <div class="dialog-body composer-body">
        <div class="row">
          <div class="field compact">
            <span class="field-label">项目</span>
            <SelectMenu v-model="projectId" :options="projectOptions" title="工作目录" />
          </div>
        </div>

        <div v-if="!projects.length" class="catalog-hint">
          还没有项目：先用其他视图跑一个会话，或到归档刷新后再回来
        </div>

        <div class="field">
          <span class="field-label">引擎</span>
          <div class="engine-tabs">
            <button
              v-for="id in ENGINES"
              :key="id"
              class="engine-tab"
              :class="{ active: engine === id, dim: engine === id && catalog && !catalog.available }"
              :title="engine === id && catalog && !catalog.available ? catalog.reason ?? '' : ''"
              @click="engine = id"
            >
              {{ ASSET_ENGINE_LABELS[id] ?? id }}
              <span v-if="engine === id && catalog && !catalog.available" class="engine-badge">
                不可用
              </span>
            </button>
          </div>
          <div v-if="catalog && !catalog.available" class="engine-reason">
            {{ catalog.reason }}
          </div>
        </div>

        <div class="row">
          <div class="field compact">
            <span class="field-label">模型</span>
            <SelectMenu
              v-model="modelKey"
              :options="modelOptions"
              :disabled="catalogLoading || !models.length"
              title="模型"
            />
          </div>
          <div class="field compact">
            <span class="field-label">推理档位</span>
            <SelectMenu
              v-model="variant"
              :options="variantOptions"
              :disabled="!variants.length"
              title="推理档位"
            />
          </div>
        </div>
        <div v-if="catalogLoading" class="catalog-hint">正在读取引擎目录…</div>

        <div class="field">
          <span class="field-label">
            首条指令（可空；输入 @ 选文件、/ 选命令、Ctrl+Enter 也可发送）
          </span>
          <div class="draft-wrap">
            <textarea
              ref="textarea"
              v-model="draft"
              class="field-input draft"
              rows="5"
              placeholder="例如：修一下登录页的回归 bug，先看 @src/login.ts"
              @input="onInput"
              @keydown="onKeydown"
              @click="onInput"
            ></textarea>
            <div v-if="popup && popup.items.length" class="composer-popup">
              <template v-if="popup.kind === 'file'">
                <button
                  v-for="(item, index) in popup.items"
                  :key="(item as FileHit).path"
                  class="popup-item"
                  :class="{ active: index === popup.active }"
                  @mousedown.prevent="pickPopup(index)"
                >
                  <span class="popup-main">{{ (item as FileHit).name }}</span>
                  <span class="popup-sub">{{ (item as FileHit).path }}</span>
                </button>
              </template>
              <template v-else>
                <button
                  v-for="(item, index) in popup.items"
                  :key="(item as EngineCommand).name"
                  class="popup-item"
                  :class="{ active: index === popup.active }"
                  @mousedown.prevent="pickPopup(index)"
                >
                  <span class="popup-main">/{{ (item as EngineCommand).name }}</span>
                  <span class="popup-sub">{{ (item as EngineCommand).description }}</span>
                </button>
              </template>
            </div>
          </div>
          <div class="draft-tools">
            <button class="ghost-btn" title="把当前分支/变更/最近提交插进指令" @click="insertContext">
              ＋ Git 上下文
            </button>
            <span v-if="attachments.length" class="attach-chips">
              <span v-for="file in attachments" :key="file.path" class="attach-chip">
                @{{ fileLabel(file.path) }}
                <button class="chip-x" title="移除" @click="removeAttachment(file)">✕</button>
              </span>
            </span>
          </div>
        </div>
      </div>

      <footer class="dialog-foot">
        <span class="foot-hint">{{ project?.worktree }}</span>
        <span class="foot-actions">
          <button class="ghost-btn" @click="emit('close')">取消</button>
          <button
            class="primary-btn"
            :disabled="saving || !project || (catalog !== null && !catalog.available)"
            @click="submit"
          >
            {{ saving ? '创建中…' : hasContent ? '创建并发送' : '创建空会话' }}
          </button>
        </span>
      </footer>
    </div>
  </div>
</template>

<style scoped src="../styles/components/SessionComposer.css"></style>
