import { describe, expect, it } from 'vitest'
import {
  RECENT_LIMIT,
  hostOf,
  pickFiles,
  pickPages,
  pushRecent,
  relativePath,
  type RecentFile,
  type RecentPage,
} from '../src/recent'

const page = (url: string, at: number, title = ''): RecentPage => ({ url, title, at })
const file = (path: string, at: number, directory = '/repo'): RecentFile => ({
  path,
  directory,
  at,
})

describe('pushRecent', () => {
  it('新的排最前', () => {
    const list = pushRecent([page('https://a.com', 1)], page('https://b.com', 2), item => item.url)
    expect(list.map(item => item.url)).toEqual(['https://b.com', 'https://a.com'])
  })

  it('同一个 key 不重复，提到最前', () => {
    const list = [page('https://a.com', 1), page('https://b.com', 2)]
    const next = pushRecent(list, page('https://a.com', 3), item => item.url)
    expect(next.map(item => item.url)).toEqual(['https://a.com', 'https://b.com'])
    expect(next[0].at).toBe(3)
    expect(list).toHaveLength(2)
  })

  it('超过上限截断（保留最近的）', () => {
    let list: RecentPage[] = []
    for (let index = 0; index < RECENT_LIMIT + 5; index += 1) {
      list = pushRecent(list, page(`https://site${index}.com`, index), item => item.url)
    }
    expect(list).toHaveLength(RECENT_LIMIT)
    expect(list[0].url).toBe(`https://site${RECENT_LIMIT + 4}.com`)
  })

  it('空 key 原样返回（不把脏数据塞进列表）', () => {
    const list = [page('https://a.com', 1)]
    expect(pushRecent(list, page('', 2), item => item.url)).toBe(list)
  })
})

describe('排序与筛选', () => {
  it('pickPages 按时间倒序并截断', () => {
    const list = [page('https://a.com', 1), page('https://b.com', 9), page('https://c.com', 5)]
    expect(pickPages(list, 2).map(item => item.url)).toEqual(['https://b.com', 'https://c.com'])
    // 不改动入参
    expect(list.map(item => item.url)).toEqual(['https://a.com', 'https://b.com', 'https://c.com'])
  })

  it('pickFiles 先本项目、再其它项目（各自按时间倒序）', () => {
    const items = [
      file('/repo/a.ts', 1),
      file('/other/b.ts', 9, '/other'),
      file('/repo/c.ts', 5),
    ]
    expect(pickFiles(items, '/repo').map(item => item.path)).toEqual([
      '/repo/c.ts',
      '/repo/a.ts',
      '/other/b.ts',
    ])
    // 没有当前项目就纯按时间
    expect(pickFiles(items, null).map(item => item.path)).toEqual([
      '/other/b.ts',
      '/repo/c.ts',
      '/repo/a.ts',
    ])
  })
})

describe('显示用的格式化', () => {
  it('hostOf 取主机名', () => {
    expect(hostOf('https://doc.rust-lang.org/std/?q=1')).toBe('doc.rust-lang.org')
    expect(hostOf('not a url')).toBe('not a url')
  })

  it('relativePath 去掉项目目录前缀', () => {
    expect(relativePath('/repo/src/a.ts', '/repo')).toBe('src/a.ts')
    expect(relativePath('/repo/src/a.ts', '/repo/')).toBe('src/a.ts')
    expect(relativePath('/elsewhere/a.ts', '/repo')).toBe('/elsewhere/a.ts')
    expect(relativePath('/repo/src/a.ts', null)).toBe('/repo/src/a.ts')
  })
})
