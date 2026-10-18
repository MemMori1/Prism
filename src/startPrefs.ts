/**
 * 新建会话的「上次选择」记忆：上次用的 Agent 与工作目录。
 *
 * 纯前端偏好，写在 localStorage，不落引擎档案。
 * 目录按路径存（同 projectPins 的理由：各引擎的 project id 不同，只有路径稳定）。
 * 读取一律容错：坏数据、隐私模式都当「没记过」，不抛异常。
 */
import { computed, ref } from 'vue'

export const START_PREFS_KEY = 'prism-start-prefs'

export interface StartPrefs {
  /** 上次创建会话用的 Agent id。 */
  engine: string | null
  /** 上次用的工作目录（绝对路径）。 */
  directory: string | null
}

function clean(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value : null
}

/** 解析持久化内容；坏数据一律当没记过。 */
export function parseStartPrefs(raw: string | null): StartPrefs {
  if (!raw) return { engine: null, directory: null }
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!parsed || typeof parsed !== 'object') return { engine: null, directory: null }
    const record = parsed as Record<string, unknown>
    return { engine: clean(record.engine), directory: clean(record.directory) }
  } catch {
    return { engine: null, directory: null }
  }
}

export function serializeStartPrefs(prefs: StartPrefs): string {
  return JSON.stringify({ engine: prefs.engine, directory: prefs.directory })
}

/** 带兜底的读取（storage 可注入，便于单测）。 */
export function readStartPrefs(storage?: Pick<Storage, 'getItem'> | null): StartPrefs {
  try {
    const target = storage ?? (typeof localStorage === 'undefined' ? null : localStorage)
    if (!target) return { engine: null, directory: null }
    return parseStartPrefs(target.getItem(START_PREFS_KEY))
  } catch {
    return { engine: null, directory: null }
  }
}

const prefs = ref<StartPrefs>(readStartPrefs())

/** 上次用的 Agent（没记过时为 null）。 */
export const lastStartEngine = computed(() => prefs.value.engine)

/** 上次用的工作目录（没记过时为 null）。 */
export const lastStartDirectory = computed(() => prefs.value.directory)

/** 记住这次的选择（只覆盖传了的字段；空值不算覆盖）。 */
export function rememberStart(patch: Partial<StartPrefs>): void {
  const next: StartPrefs = {
    engine: clean(patch.engine) ?? prefs.value.engine,
    directory: clean(patch.directory) ?? prefs.value.directory,
  }
  prefs.value = next
  try {
    localStorage.setItem(START_PREFS_KEY, serializeStartPrefs(next))
  } catch {
    // 隐私模式等写入失败：本次会话内仍然生效，只是不持久化。
  }
}
