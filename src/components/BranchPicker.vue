<script setup lang="ts">
/**
 * Git 分支切换：会话头部一个小 chip 显示当前分支，点开是弹层——
 * 搜/选已有本地分支，或基于当前 HEAD 新建一个再切过去。
 * 会破坏本地改动的切换由 git 自己拒绝，错误原文直接显示在这里。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { relativeTime } from '../format'
import type { GitBranch, GitBranches } from '../types'

const props = withDefaults(
  defineProps<{
    directory: string
    disabled?: boolean
  }>(),
  { disabled: false },
)

const emit = defineEmits<{
  (e: 'changed', branch: string): void
  (e: 'error', message: string): void
}>()

const state = ref<GitBranches | null>(null)
const loading = ref(false)
const open = ref(false)
const query = ref('')
const active = ref(0)
const switching = ref<string | null>(null)
const newName = ref('')
const creating = ref(false)
const failure = ref<string | null>(null)

const trigger = ref<HTMLButtonElement | null>(null)
const popup = ref<HTMLElement | null>(null)
const search = ref<HTMLInputElement | null>(null)

const pos = ref({ left: 0, top: 0, bottom: null as number | null, width: 300, maxHeight: 340 })

const current = computed(() => state.value?.current ?? '')
const dirty = computed(() => state.value?.dirty ?? 0)
const branches = computed<GitBranch[]>(() => state.value?.branches ?? [])
const filtered = computed(() => {
  const term = query.value.trim().toLowerCase()
  if (!term) return branches.value
  return branches.value.filter(
    branch =>
      branch.name.toLowerCase().includes(term) || branch.subject.toLowerCase().includes(term),
  )
})

const popupStyle = computed(() => {
  const style: Record<string, string> = {
    left: `${pos.value.left}px`,
    width: `${pos.value.width}px`,
    maxHeight: `${pos.value.maxHeight}px`,
  }
  if (pos.value.bottom != null) style.bottom = `${pos.value.bottom}px`
  else style.top = `${pos.value.top}px`
  return style
})

async function refresh(): Promise<void> {
  if (!props.directory) return
  loading.value = true
  try {
    state.value = await api.gitBranches(props.directory)
  } catch {
    // 非 Git 仓库 / 目录已删除：不显示 chip，也不打扰用户。
    state.value = null
  } finally {
    loading.value = false
  }
}

function measure(): void {
  const el = trigger.value
  if (!el) return
  const rect = el.getBoundingClientRect()
  const margin = 8
  const gap = 6
  const width = Math.min(Math.max(rect.width, 300), window.innerWidth - margin * 2)
  const below = window.innerHeight - rect.bottom - margin
  const above = rect.top - margin
  const flip = below < 240 && above > below
  const left = Math.min(Math.max(margin, rect.left), window.innerWidth - width - margin)
  pos.value = {
    left,
    width,
    maxHeight: Math.min(340, Math.max(200, (flip ? above : below) - gap)),
    top: rect.bottom + gap,
    bottom: flip ? window.innerHeight - rect.top + gap : null,
  }
}

async function openMenu(): Promise<void> {
  if (open.value || props.disabled || !state.value) return
  open.value = true
  query.value = ''
  failure.value = null
  active.value = Math.max(0, branches.value.findIndex(branch => branch.name === current.value))
  measure()
  void refresh()
  await nextTick()
  measure()
  search.value?.focus()
}

function closeMenu(refocus = false): void {
  if (!open.value) return
  open.value = false
  if (refocus) trigger.value?.focus()
}

function toggle(): void {
  if (open.value) closeMenu()
  else void openMenu()
}

function pick(index: number): void {
  const branch = filtered.value[index]
  if (!branch) return
  if (branch.name === current.value) {
    closeMenu(true)
    return
  }
  void switchTo(branch.name)
}

async function switchTo(name: string): Promise<void> {
  if (switching.value || creating.value) return
  switching.value = name
  failure.value = null
  try {
    const snapshot = await api.gitSwitchBranch(props.directory, name, false)
    emit('changed', snapshot.branch ?? name)
    closeMenu(true)
    await refresh()
  } catch (cause) {
    failure.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    switching.value = null
  }
}

async function createAndSwitch(): Promise<void> {
  const name = newName.value.trim()
  if (!name || switching.value || creating.value) return
  creating.value = true
  failure.value = null
  try {
    const snapshot = await api.gitSwitchBranch(props.directory, name, true)
    emit('changed', snapshot.branch ?? name)
    newName.value = ''
    closeMenu(true)
    await refresh()
  } catch (cause) {
    failure.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    creating.value = false
  }
}

function move(step: number): void {
  const total = filtered.value.length
  if (!total) return
  active.value = (active.value + step + total) % total
  void nextTick(() => {
    popup.value?.querySelectorAll<HTMLElement>('.bp-item')[active.value]?.scrollIntoView({
      block: 'nearest',
    })
  })
}

function onSearchKeydown(event: KeyboardEvent): void {
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    move(1)
    return
  }
  if (event.key === 'ArrowUp') {
    event.preventDefault()
    move(-1)
    return
  }
  if (event.key === 'Enter') {
    event.preventDefault()
    pick(active.value)
  }
}

function onPointerDown(event: PointerEvent): void {
  if (!open.value) return
  const target = event.target as Node | null
  if (!target) return
  if (trigger.value?.contains(target) || popup.value?.contains(target)) return
  closeMenu()
}

function onKeydown(event: KeyboardEvent): void {
  if (!open.value) return
  if (event.key === 'Escape') {
    event.preventDefault()
    closeMenu(true)
  }
}

watch(query, () => {
  active.value = 0
})

watch(
  () => props.directory,
  () => {
    closeMenu()
    void refresh()
  },
)

function onResize(): void {
  closeMenu()
}

onMounted(() => {
  window.addEventListener('pointerdown', onPointerDown, true)
  window.addEventListener('keydown', onKeydown)
  window.addEventListener('resize', onResize)
  void refresh()
})

onBeforeUnmount(() => {
  window.removeEventListener('pointerdown', onPointerDown, true)
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('resize', onResize)
})
</script>

<template>
  <span v-if="state" class="branch-pick">
    <button
      ref="trigger"
      type="button"
      class="branch-chip"
      :class="{ on: open }"
      :disabled="disabled"
      :title="current ? `当前分支：${current}${dirty ? ` · ${dirty} 处未提交改动` : ''}（点击切换）` : '切换分支'"
      aria-haspopup="dialog"
      :aria-expanded="open"
      @click.stop="toggle"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <circle cx="6.5" cy="5.5" r="2.2" />
        <circle cx="6.5" cy="18.5" r="2.2" />
        <circle cx="17.5" cy="8.5" r="2.2" />
        <path d="M6.5 7.7v8.6M17.5 10.7c0 3-2.6 4.4-6 4.8" />
      </svg>
      <span class="branch-name">{{ current || 'detached' }}</span>
      <span v-if="dirty" class="branch-dot" :title="`${dirty} 处未提交改动`" />
    </button>

    <Teleport to="body">
      <div v-if="open" ref="popup" class="bp-pop" :style="popupStyle" role="dialog" aria-label="切换分支">
        <div class="bp-head">
          <span class="bp-title">切换分支</span>
          <span v-if="dirty" class="bp-dirty">{{ dirty }} 处未提交改动</span>
        </div>

        <label class="bp-search">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <circle cx="11" cy="11" r="7" />
            <path d="m20 20-3.5-3.5" />
          </svg>
          <input
            ref="search"
            v-model="query"
            type="text"
            placeholder="筛选分支…"
            spellcheck="false"
            @keydown="onSearchKeydown"
          />
        </label>

        <div class="bp-list">
          <button
            v-for="(branch, index) in filtered"
            :key="branch.name"
            type="button"
            class="bp-item"
            :class="{ active: index === active, current: branch.name === current }"
            @pointerenter="active = index"
            @click="pick(index)"
          >
            <span class="bp-item-main">
              <span class="bp-item-name">{{ branch.name }}</span>
              <span class="bp-item-sub">{{ branch.subject || '（无提交说明）' }}</span>
            </span>
            <span class="bp-item-meta">
              <span v-if="switching === branch.name" class="bp-busy">切换中…</span>
              <template v-else>
                <span v-if="branch.time" class="bp-item-time">{{ relativeTime(branch.time * 1000) }}</span>
                <span v-if="branch.name === current" class="bp-current">当前</span>
              </template>
            </span>
          </button>
          <div v-if="!filtered.length" class="bp-empty">
            {{ loading ? '读取分支…' : '没有匹配的分支' }}
          </div>
        </div>

        <div v-if="failure" class="bp-error">{{ failure }}</div>

        <div class="bp-new">
          <input
            v-model="newName"
            type="text"
            placeholder="新建分支并切换（基于当前 HEAD）"
            spellcheck="false"
            @keydown.enter.prevent="createAndSwitch"
          />
          <button
            type="button"
            class="bp-new-btn"
            :disabled="!newName.trim() || creating || !!switching"
            @click="createAndSwitch"
          >
            {{ creating ? '创建中…' : '创建' }}
          </button>
        </div>
      </div>
    </Teleport>
  </span>
</template>

<style scoped src="../styles/components/BranchPicker.css"></style>
