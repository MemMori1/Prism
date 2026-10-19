import { MODEL_ICON_SHAPES, type ModelIconShape } from './modelIconData'

/**
 * 模型服务 → 小图标 + 上下文窗口。
 *
 * 图标用**官方标志的单色版**（形状见 modelIconData.ts），不再自己"画个像的"：
 * 鲸鱼就该是 DeepSeek 那只鲸鱼，星芒就该是 Claude 那枚星芒，自己捏的形状一眼假。
 * 只有「通用模型」这种没有官方形状的才回落到描边立方体。
 */

export type ModelFamily =
  | 'deepseek'
  | 'openai'
  | 'anthropic'
  | 'google'
  | 'ollama'
  | 'moonshot'
  | 'qwen'
  | 'zhipu'
  | 'xai'
  | 'mistral'
  | 'meta'
  | 'generic'

/**
 * 模型键形如 `deepseek/deepseek-chat`；没有斜杠时按模型名前缀猜。
 *
 * 判定顺序：**先看模型名，再看服务**。因为聚合类服务（ollama / openrouter / 自建网关）
 * 的 provider 段指的是"从哪连的"，模型叫什么才是用户认的东西——`ollama/qwen2.5`
 * 应该显示通义的图标，而不是一个"本地"图标。
 */
/**
 * 模型键 → 服务段 + 模型段。
 *
 * 两种写法都要认：本应用存的是 `provider::model`（见 composer.ts 的 modelKeyOf），
 * 各家 CLI 给的是 `provider/model`。`::` 优先，因为它不会出现在模型名里。
 */
export function splitModelKey(key: string | null | undefined): { provider: string; model: string } {
  const text = (key ?? '').trim().toLowerCase()
  if (!text) return { provider: '', model: '' }
  const double = text.lastIndexOf('::')
  if (double > 0) return { provider: text.slice(0, double), model: text.slice(double + 2) }
  const slash = text.indexOf('/')
  if (slash > 0) return { provider: text.slice(0, slash), model: text.slice(slash + 1) }
  return { provider: '', model: text }
}

export function modelFamily(key: string | null | undefined): ModelFamily {
  const { provider, model } = splitModelKey(key)
  if (!model) return 'generic'

  if (/deepseek|深度求索/.test(model)) return 'deepseek'
  if (/claude|anthropic/.test(model)) return 'anthropic'
  if (/gemini|gemma|palm/.test(model)) return 'google'
  if (/kimi|moonshot/.test(model)) return 'moonshot'
  if (/qwen|qwq|tongyi/.test(model)) return 'qwen'
  if (/glm|zhipu|chatglm/.test(model)) return 'zhipu'
  if (/grok/.test(model)) return 'xai'
  if (/mistral|mixtral|codestral/.test(model)) return 'mistral'
  if (/llama/.test(model)) return 'meta'
  if (/gpt|^o[134]|chatgpt|openai/.test(model)) return 'openai'

  // 模型名认不出来，再看它挂在哪个服务下。
  if (/deepseek/.test(provider)) return 'deepseek'
  if (/anthropic/.test(provider)) return 'anthropic'
  if (/google|vertex/.test(provider)) return 'google'
  if (/ollama|llama\.cpp|local/.test(provider)) return 'ollama'
  if (/moonshot/.test(provider)) return 'moonshot'
  if (/qwen|dashscope|aliyun/.test(provider)) return 'qwen'
  if (/zhipu|bigmodel/.test(provider)) return 'zhipu'
  if (/xai/.test(provider)) return 'xai'
  if (/mistral/.test(provider)) return 'mistral'
  if (/meta/.test(provider)) return 'meta'
  if (/openai/.test(provider)) return 'openai'
  return 'generic'
}

/** 服务的中文名（tooltip 用）。 */
export const MODEL_FAMILY_LABEL: Record<ModelFamily, string> = {
  deepseek: 'DeepSeek',
  openai: 'OpenAI',
  anthropic: 'Claude',
  google: 'Gemini',
  ollama: 'Ollama',
  moonshot: 'Kimi',
  qwen: '通义千问',
  zhipu: '智谱',
  xai: 'Grok',
  mistral: 'Mistral',
  meta: 'Llama',
  generic: '模型',
}

/**
 * 每种服务用哪套形状。
 *
 * `fill` 的是官方标志（单色化后按 currentColor 上色），`stroke` 的是自己描的
 * 通用立方体——只有它没有"官方形状"可言。
 */
export const MODEL_ICONS: Record<ModelFamily, ModelIconShape> = {
  deepseek: MODEL_ICON_SHAPES.deepseek,
  openai: MODEL_ICON_SHAPES.openai,
  anthropic: MODEL_ICON_SHAPES.anthropic,
  google: MODEL_ICON_SHAPES.google,
  ollama: MODEL_ICON_SHAPES.ollama,
  moonshot: MODEL_ICON_SHAPES.moonshot,
  qwen: MODEL_ICON_SHAPES.qwen,
  zhipu: MODEL_ICON_SHAPES.zhipu,
  xai: MODEL_ICON_SHAPES.xai,
  mistral: MODEL_ICON_SHAPES.mistral,
  meta: MODEL_ICON_SHAPES.meta,
  generic: {
    mode: 'stroke',
    paths: ['M12 4.2 19 8.3v7.4l-7 4.1-7-4.1V8.3Z', 'M5 8.3 12 12.4l7-4.1', 'M12 12.4v7.4'],
  },
}

/**
 * 常见模型的上下文窗口（token）。
 *
 * 只用于「没有引擎数据的会话」估算分母——codex 会直接给 `contextWindow`，那边优先。
 * 命中不了就返回 null：宁可不画进度，也不要编一个分母。
 */
const CONTEXT_WINDOWS: Array<[RegExp, number]> = [
  [/^gpt-4\.1|^gpt-5/, 1_000_000],
  [/^gpt-4o|^gpt-4-turbo|^gpt-4\.5/, 128_000],
  [/^o[134]/, 200_000],
  [/claude/, 200_000],
  [/gemini/, 1_000_000],
  [/deepseek/, 128_000],
  [/qwen|qwq/, 128_000],
  [/glm/, 128_000],
  [/kimi|moonshot/, 256_000],
  [/grok/, 256_000],
  [/llama|mistral|mixtral|codestral/, 128_000],
]

export function contextWindowOf(key: string | null | undefined): number | null {
  const { model } = splitModelKey(key)
  if (!model) return null
  for (const [pattern, window] of CONTEXT_WINDOWS) {
    if (pattern.test(model)) return window
  }
  return null
}
