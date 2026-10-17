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

<style scoped src="../styles/components/TagPicker.css"></style>
