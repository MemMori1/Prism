import { describe, expect, it } from 'vitest'
import {
  parseStartPrefs,
  readStartPrefs,
  serializeStartPrefs,
  START_PREFS_KEY,
} from '../src/startPrefs'

describe('parseStartPrefs', () => {
  it('解析正常内容', () => {
    expect(parseStartPrefs('{"engine":"native","directory":"/tmp/demo"}')).toEqual({
      engine: 'native',
      directory: '/tmp/demo',
    })
  })

  it('坏数据一律当没记过（不抛异常）', () => {
    expect(parseStartPrefs(null)).toEqual({ engine: null, directory: null })
    expect(parseStartPrefs('')).toEqual({ engine: null, directory: null })
    expect(parseStartPrefs('{')).toEqual({ engine: null, directory: null })
    expect(parseStartPrefs('[]')).toEqual({ engine: null, directory: null })
    expect(parseStartPrefs('{"engine":123,"directory":{}}')).toEqual({
      engine: null,
      directory: null,
    })
  })

  it('空字符串 / 纯空白不算记过', () => {
    expect(parseStartPrefs('{"engine":"  ","directory":""}')).toEqual({
      engine: null,
      directory: null,
    })
  })

  it('只记了一半也算数', () => {
    expect(parseStartPrefs('{"engine":"codex"}')).toEqual({ engine: 'codex', directory: null })
  })

  it('序列化后能原样解析回来', () => {
    const prefs = { engine: 'opencode', directory: '/a/b' }
    expect(parseStartPrefs(serializeStartPrefs(prefs))).toEqual(prefs)
  })
})

describe('readStartPrefs', () => {
  const storage = (value: string | null) => ({
    getItem: (key: string) => (key === START_PREFS_KEY ? value : null),
  })

  it('从 storage 读', () => {
    expect(readStartPrefs(storage('{"engine":"native"}')).engine).toBe('native')
  })

  it('没有 storage（隐私模式 / 单测环境）返回空，不炸', () => {
    expect(readStartPrefs(null)).toEqual({ engine: null, directory: null })
  })

  it('storage 抛错时返回空', () => {
    const broken = {
      getItem() {
        throw new Error('denied')
      },
    }
    expect(readStartPrefs(broken)).toEqual({ engine: null, directory: null })
  })
})
