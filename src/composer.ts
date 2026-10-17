/**
 * 组合器纯逻辑：@ 文件 / / 命令触发检测、补全应用、! shell 直跑、
 * 斜杠命令调用解析（全部纯函数，可单测）。
 */
import type { EngineModel, TimelineRecord } from './types'

export interface Trigger {
  kind: 'file' | 'command'
  /** 触发词起始位置（@ 或 / 的下标）。 */
  start: number
  /** 触发词之后、光标之前的检索词。 */
  term: string
}

/**
 * 在光标处检测触发词：
 * - 文件：`@` 前面是行首/空白，且 `@` 之后到光标无空白；
 * - 命令：行（可含前导空白）以 `/` 开头且之后到光标无空白。
 */
export function detectTrigger(text: string, caret: number): Trigger | null {
  if (caret < 0 || caret > text.length) return null
  const before = text.slice(0, caret)

  const at = before.lastIndexOf('@')
  if (at >= 0) {
    const prev = at === 0 ? '' : before[at - 1]
    const term = before.slice(at + 1)
    if ((prev === '' || /\s/.test(prev)) && !/\s/.test(term)) {
      return { kind: 'file', start: at, term }
    }
  }

  const lineStart = before.lastIndexOf('\n') + 1
  const line = before.slice(lineStart)
  const slash = /^(\s*)\/(\S*)$/.exec(line)
  if (slash) {
    return { kind: 'command', start: lineStart + slash[1].length, term: slash[2] }
  }
  return null
}

/** 应用补全：替换触发词为 replacement（自动补空格），返回新文本与新光标。 */
export function applyCompletion(
  text: string,
  trigger: Trigger,
  replacement: string,
  caret: number,
): { text: string; caret: number } {
  const inserted = replacement.endsWith(' ') ? replacement : `${replacement} `
  const next = text.slice(0, trigger.start) + inserted + text.slice(caret)
  return { text: next, caret: trigger.start + inserted.length }
}

/** `!命令` 直跑识别：返回命令文本（空串 = 只有感叹号）；不是 shell 草稿返回 null。 */
export function shellCommand(draft: string): string | null {
  const trimmed = draft.trim()
  if (!trimmed.startsWith('!')) return null
  return trimmed.slice(1).trim()
}

/** 斜杠命令调用解析：`/name 参数` 且 name 在已知命令里 → {name, text}。 */
export function commandInvocation(
  draft: string,
  commands: { name: string }[],
): { name: string; text: string } | null {
  const match = /^\/(\S+)\s*([\s\S]*)$/.exec(draft.trim())
  if (!match) return null
  const name = match[1]
  if (!commands.some(command => command.name === name)) return null
  return { name, text: match[2] }
}

/** 文件名 → 附件展示名（末段）。 */
export function fileLabel(path: string): string {
  const index = path.lastIndexOf('/')
  return index >= 0 ? path.slice(index + 1) : path
}

/** 模型 key：有提供方用 `provider::id`，否则用 id（与下拉选项值一致）。 */
export function modelKeyOf(model: Pick<EngineModel, 'provider' | 'id'>): string {
  return model.provider ? `${model.provider}::${model.id}` : model.id
}

/** 模型展示名（含提供方前缀与默认标记）。 */
export function modelLabel(model: EngineModel): string {
  const name = model.label && model.label !== model.id ? `${model.label}（${model.id}）` : model.id
  const provider = model.provider ? `${model.provider}/` : ''
  return model.isDefault ? `${provider}${name} · 默认` : `${provider}${name}`
}

/**
 * 从时间线记录反推模型 key（编码与 modelKeyOf 一致，`provider::id`）。
 * 记录里没有模型时返回 null，调用方保留原选择。
 */
export function recordModelKey(
  record: Pick<TimelineRecord, 'model' | 'modelProvider'>,
): string | null {
  const id = record.model?.trim()
  if (!id) return null
  const provider = record.modelProvider?.trim()
  return provider ? `${provider}::${id}` : id
}

/** 模型 key 的兜底展示名（catalog 未加载 / 模型已下架时用），档位附在后面。 */
export function modelKeyLabel(key: string, variant = ''): string {
  const id = key.includes('::') ? key.slice(key.indexOf('::') + 2) : key
  const base = id || '默认'
  return variant ? `${base} · ${variant}` : base
}
