import { describe, expect, it } from 'vitest'
import { MODEL_ICONS, contextWindowOf, modelFamily, splitModelKey } from '../src/modelIcons'

describe('modelFamily', () => {
  it('按服务段识别', () => {
    expect(modelFamily('deepseek/deepseek-chat')).toBe('deepseek')
    expect(modelFamily('openai/gpt-4o')).toBe('openai')
    expect(modelFamily('anthropic/claude-sonnet-4')).toBe('anthropic')
    expect(modelFamily('ollama/qwen2.5')).toBe('qwen')
    expect(modelFamily('ollama/some-local-model')).toBe('ollama')
    expect(modelFamily('moonshot/kimi-k2')).toBe('moonshot')
  })

  it('没有服务段时按模型名猜', () => {
    expect(modelFamily('deepseek-reasoner')).toBe('deepseek')
    expect(modelFamily('claude-3-7-sonnet')).toBe('anthropic')
    expect(modelFamily('gpt-4.1-mini')).toBe('openai')
    expect(modelFamily('gemini-2.0-flash')).toBe('google')
    expect(modelFamily('glm-4.6')).toBe('zhipu')
    expect(modelFamily('grok-4')).toBe('xai')
  })

  it('也认本应用的 `provider::model` 写法', () => {
    expect(modelFamily('deepseek::deepseek-chat')).toBe('deepseek')
    expect(modelFamily('openai::gpt-4o')).toBe('openai')
    expect(modelFamily('ollama::qwen2.5')).toBe('qwen')
    expect(modelFamily('anthropic::claude-sonnet-4')).toBe('anthropic')
  })

  it('认不出来＝通用', () => {
    expect(modelFamily('')).toBe('generic')
    expect(modelFamily(null)).toBe('generic')
    expect(modelFamily('my-finetune-v3')).toBe('generic')
  })
})

describe('MODEL_ICONS', () => {
  const families = [
    'deepseek',
    'openai',
    'anthropic',
    'google',
    'ollama',
    'moonshot',
    'qwen',
    'zhipu',
    'xai',
    'mistral',
    'meta',
    'generic',
  ] as const

  it('每个服务都有形状，且 path 数据非空', () => {
    for (const family of families) {
      expect(MODEL_ICONS[family].paths.length).toBeGreaterThan(0)
      expect(MODEL_ICONS[family].paths.every(d => d.trim().length > 10)).toBe(true)
    }
  })

  it('品牌用官方填充形状，只有通用模型是描边', () => {
    for (const family of families) {
      expect(MODEL_ICONS[family].mode).toBe(family === 'generic' ? 'stroke' : 'fill')
    }
  })

  it('官方标志都是完整路径（从句首移动开始，不是半截）', () => {
    for (const family of families) {
      if (MODEL_ICONS[family].mode !== 'fill') continue
      for (const d of MODEL_ICONS[family].paths) {
        expect(d).toMatch(/^[Mm]/)
        expect(d.length).toBeGreaterThan(40)
        // 只允许画图指令，混进 HTML/JS 一眼能看出来
        expect(d).not.toMatch(/[<>{}"']/)
      }
    }
  })
})

describe('contextWindowOf', () => {
  it('常见模型给得出分母', () => {
    expect(contextWindowOf('anthropic/claude-sonnet-4')).toBe(200_000)
    expect(contextWindowOf('deepseek/deepseek-chat')).toBe(128_000)
    expect(contextWindowOf('openai/gpt-4o')).toBe(128_000)
    expect(contextWindowOf('openai/gpt-4.1')).toBe(1_000_000)
  })

  it('认不出来就返回 null，不编分母', () => {
    expect(contextWindowOf('my-own-model')).toBeNull()
    expect(contextWindowOf('')).toBeNull()
    expect(contextWindowOf(null)).toBeNull()
  })
})

describe('splitModelKey', () => {
  it('`::` 与 `/` 两种分隔都拆开', () => {
    expect(splitModelKey('deepseek::deepseek-chat')).toEqual({
      provider: 'deepseek',
      model: 'deepseek-chat',
    })
    expect(splitModelKey('deepseek/deepseek-chat')).toEqual({
      provider: 'deepseek',
      model: 'deepseek-chat',
    })
  })

  it('没有 provider ＝ 全是模型名', () => {
    expect(splitModelKey('gpt-4o')).toEqual({ provider: '', model: 'gpt-4o' })
    expect(splitModelKey('')).toEqual({ provider: '', model: '' })
    expect(splitModelKey(null)).toEqual({ provider: '', model: '' })
  })

  it('模型名里带斜杠时（自建网关）不会把模型段切掉', () => {
    expect(splitModelKey('openrouter::meta-llama/llama-3.1-70b')).toEqual({
      provider: 'openrouter',
      model: 'meta-llama/llama-3.1-70b',
    })
  })
})
