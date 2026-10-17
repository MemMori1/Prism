import { ref } from 'vue'
import { api } from './api'
import type { AnnotationStore, SessionAnnotation, TagDef } from './types'

/**
 * 档案增补（星标 / 标签 / 自定义标题）的前端缓存。
 *
 * 后端返回全量存储，任何一处修改后整体替换 —— 前端只读缓存、不做局部合并，
 * 避免两侧状态漂移。
 */

/** 新建标签时的默认调色板（顺位取色）。 */
export const TAG_COLORS = [
  '#6d5ae6',
  '#1d4ed8',
  '#15803d',
  '#b45309',
  '#b91c1c',
  '#7c3aed',
  '#0f766e',
  '#64748b',
]

const store = ref<AnnotationStore>({ version: 1, tags: [], sessions: {} })
let loaded = false
let loading: Promise<void> | null = null

/** 首次进入档案界面时拉取；失败静默（不阻塞浏览，写操作会重新暴露错误）。 */
export function ensureAnnotations(): Promise<void> {
  if (loaded) return Promise.resolve()
  if (!loading) {
    loading = api
      .annotationsGet()
      .then(data => {
        store.value = data
      })
      .catch(() => {})
      .finally(() => {
        loaded = true
        loading = null
      })
  }
  return loading
}

export function annotationFor(sessionId: string): SessionAnnotation | null {
  return store.value.sessions[sessionId] ?? null
}

/** 展示标题：自定义标题优先，其次引擎标题。 */
export function titleOf(sessionId: string, fallback: string | null | undefined): string {
  return annotationFor(sessionId)?.title || fallback || '未命名会话'
}

export function isStarred(sessionId: string): boolean {
  return annotationFor(sessionId)?.starred ?? false
}

export function tagIdsOf(sessionId: string): string[] {
  return annotationFor(sessionId)?.tags ?? []
}

/** 某会话的标签（保序；已删标签自动消失）。 */
export function tagsOf(sessionId: string): TagDef[] {
  const ids = tagIdsOf(sessionId)
  if (!ids.length) return []
  const byId = new Map(store.value.tags.map(tag => [tag.id, tag]))
  return ids.map(id => byId.get(id)).filter((tag): tag is TagDef => Boolean(tag))
}

export function allTags(): TagDef[] {
  return store.value.tags
}

/** 名称冲突检查（用于「改成已有名字 = 合并」的判定）。 */
export function tagNameTaken(name: string, exceptId?: string): TagDef | null {
  const trimmed = name.trim()
  return (
    store.value.tags.find(tag => tag.name === trimmed && tag.id !== exceptId) ?? null
  )
}

async function apply(action: Promise<AnnotationStore>): Promise<void> {
  store.value = await action
}

export function setStar(sessionId: string, starred: boolean): Promise<void> {
  return apply(api.annotationUpdate(sessionId, { starred }))
}

/** 设置自定义标题（空串 = 恢复引擎标题）。 */
export function setTitle(sessionId: string, title: string): Promise<void> {
  return apply(api.annotationUpdate(sessionId, { title }))
}

export function setTags(sessionId: string, tags: string[]): Promise<void> {
  return apply(api.annotationUpdate(sessionId, { tags }))
}

export function createTag(name: string, color: string): Promise<void> {
  return apply(api.tagCreate(name, color))
}

export function renameTag(id: string, name: string): Promise<void> {
  return apply(api.tagUpdate(id, { name }))
}

export function recolorTag(id: string, color: string): Promise<void> {
  return apply(api.tagUpdate(id, { color }))
}

export function deleteTag(id: string): Promise<void> {
  return apply(api.tagDelete(id))
}

export function mergeTag(sourceId: string, targetId: string): Promise<void> {
  return apply(api.tagMerge(sourceId, targetId))
}
