<script setup lang="ts">
import { computed, ref } from 'vue'
import {
  TAG_COLORS,
  allTags,
  createTag,
  deleteTag,
  mergeTag,
  recolorTag,
  renameTag,
  setTags,
  tagIdsOf,
  tagNameTaken,
} from '../annotations'

const props = defineProps<{
  sessionId: string
}>()

const emit = defineEmits<{
  (e: 'close'): void
}>()

const query = ref('')
const error = ref('')
/** 正在重命名的标签 id 与草稿。 */
const renamingId = ref<string | null>(null)
const renameDraft = ref('')
/** 待确认删除的标签 id（点两次才真正删）。 */
const confirmingId = ref<string | null>(null)

const selectedIds = computed(() => tagIdsOf(props.sessionId))

const filtered = computed(() => {
  const keyword = query.value.trim().toLowerCase()
  if (!keyword) return allTags()
  return allTags().filter(tag => tag.name.toLowerCase().includes(keyword))
})

async function run(action: () => Promise<void>) {
  error.value = ''
  try {
    await action()
  } catch (cause) {
    error.value = String(cause)
  }
}

async function toggleTag(id: string) {
  const current = selectedIds.value
  const next = current.includes(id)
    ? current.filter(tagId => tagId !== id)
    : [...current, id]
  await run(() => setTags(props.sessionId, next))
}

/** 回车：命中已有标签则切换；否则新建（顺位取色）并挂上。 */
async function onQueryEnter() {
  const name = query.value.trim()
  if (!name) return
  const existing = allTags().find(tag => tag.name === name)
  if (existing) {
    query.value = ''
    await toggleTag(existing.id)
    return
  }
  await run(async () => {
    await createTag(name, TAG_COLORS[allTags().length % TAG_COLORS.length])
    const created = allTags().find(tag => tag.name === name)
    if (created && !selectedIds.value.includes(created.id)) {
      await setTags(props.sessionId, [...selectedIds.value, created.id])
    }
  })
  query.value = ''
}

function startRename(id: string, name: string) {
  confirmingId.value = null
  renamingId.value = id
  renameDraft.value = name
}

/** 提交改名：改成已有标签名 = 合并到该标签。 */
async function commitRename(id: string) {
  const name = renameDraft.value.trim()
  renamingId.value = null
  if (!name) return
  const clash = tagNameTaken(name, id)
  if (clash) {
    await run(() => mergeTag(id, clash.id))
    return
  }
  await run(() => renameTag(id, name))
}

async function onDeleteClick(id: string) {
  if (confirmingId.value !== id) {
    confirmingId.value = id
    renamingId.value = null
    return
  }
  confirmingId.value = null
  await run(() => deleteTag(id))
}

function cycleColor(id: string, current: string) {
  const index = TAG_COLORS.indexOf(current.toLowerCase())
  const next = TAG_COLORS[(index + 1) % TAG_COLORS.length]
  void run(() => recolorTag(id, next))
}
</script>

<template>
  <div class="tag-picker" @click.stop>
    <input
      v-model="query"
      class="tag-query"
      placeholder="搜索 / 输入新名称后回车"
      spellcheck="false"
      @keydown.enter="onQueryEnter"
      @keydown.esc="emit('close')"
    />

    <div class="tag-picker-list">
      <div v-for="tag in filtered" :key="tag.id" class="tag-row">
        <input
          v-if="renamingId === tag.id"
          v-model="renameDraft"
          class="tag-rename"
          spellcheck="false"
          @keydown.enter="commitRename(tag.id)"
          @keydown.esc="renamingId = null"
        />
        <template v-else>
          <span
            class="tag-check"
            :class="{ on: selectedIds.includes(tag.id) }"
            role="checkbox"
            :aria-checked="selectedIds.includes(tag.id)"
            @click="toggleTag(tag.id)"
          >
            {{ selectedIds.includes(tag.id) ? '✓' : '' }}
          </span>
          <span
            class="tag-dot"
            :style="{ background: tag.color }"
            title="点击换色"
            @click="cycleColor(tag.id, tag.color)"
          />
          <span class="tag-name" @click="toggleTag(tag.id)">{{ tag.name }}</span>
          <span class="tag-row-actions">
            <span
              class="tag-action"
              title="重命名（改成已有名字则自动合并）"
              @click="startRename(tag.id, tag.name)"
            >
              ✎
            </span>
            <span
              class="tag-action"
              :class="{ danger: confirmingId === tag.id }"
              :title="confirmingId === tag.id ? '再点一次确认删除' : '删除标签'"
              @click="onDeleteClick(tag.id)"
            >
              {{ confirmingId === tag.id ? '确认?' : '✕' }}
            </span>
          </span>
        </template>
      </div>
      <div v-if="!filtered.length" class="tag-empty">
        {{ allTags().length ? '没有匹配的标签' : '还没有标签，输入名称回车新建' }}
      </div>
    </div>

    <div v-if="error" class="tag-error">{{ error }}</div>
    <div class="tag-picker-foot">
      <span class="tag-hint">回车新建 · ✎ 改名（同名合并）· 点色块换色</span>
      <button class="tag-done" @click="emit('close')">完成</button>
    </div>
  </div>
</template>

<style scoped>
.tag-picker {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 40;
  width: 286px;
  display: flex;
  flex-direction: column;
  background: var(--panel);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius);
  box-shadow: 0 10px 32px rgb(18 16 34 / 0.16);
  overflow: hidden;
}

.tag-query {
  flex: none;
  margin: 9px 10px 6px;
  height: 26px;
  padding: 0 8px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  color: var(--text);
  font-size: 12px;
}

.tag-query:focus {
  outline: none;
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
}

.tag-picker-list {
  max-height: 240px;
  overflow-y: auto;
  padding: 2px 6px 6px;
}

.tag-row {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 4px 5px;
  border-radius: 6px;
}

.tag-row:hover {
  background: var(--panel-2);
}

.tag-check {
  flex: none;
  width: 15px;
  height: 15px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--border-strong);
  border-radius: 4px;
  font-size: 10px;
  color: #fff;
  cursor: pointer;
  user-select: none;
}

.tag-check.on {
  background: var(--accent);
  border-color: var(--accent);
}

.tag-dot {
  flex: none;
  width: 10px;
  height: 10px;
  border-radius: 50%;
  cursor: pointer;
}

.tag-name {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  cursor: pointer;
}

.tag-row-actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 4px;
  opacity: 0;
}

.tag-row:hover .tag-row-actions {
  opacity: 1;
}

.tag-action {
  padding: 1px 5px;
  border-radius: 4px;
  font-size: 11px;
  color: var(--faint);
  cursor: pointer;
}

.tag-action:hover {
  color: var(--text);
  background: var(--panel);
}

.tag-action.danger {
  color: var(--red);
  font-weight: 600;
}

.tag-rename {
  flex: 1;
  height: 22px;
  padding: 0 6px;
  border: 1px solid color-mix(in srgb, var(--accent) 55%, transparent);
  border-radius: 5px;
  background: var(--panel-2);
  color: var(--text);
  font-size: 12px;
}

.tag-rename:focus {
  outline: none;
}

.tag-empty {
  padding: 10px 8px;
  color: var(--faint);
  font-size: 11.5px;
}

.tag-error {
  padding: 4px 10px;
  color: var(--red);
  font-size: 11px;
}

.tag-picker-foot {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px 8px;
  border-top: 1px solid var(--border);
}

.tag-hint {
  flex: 1;
  color: var(--faint);
  font-size: 10.5px;
}

.tag-done {
  flex: none;
  padding: 3px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 11.5px;
  cursor: pointer;
}

.tag-done:hover {
  color: var(--text);
  border-color: var(--border-strong);
}
</style>
