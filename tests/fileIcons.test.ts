import { describe, expect, it } from 'vitest'
import { FILE_ICON_PATHS, fileIconKind } from '../src/fileIcons'

describe('fileIconKind', () => {
  it('代码 / 样式 / 数据 / markdown / 图片 / 脚本 / 文本', () => {
    expect(fileIconKind('api.ts')).toBe('code')
    expect(fileIconKind('Pane.vue')).toBe('code')
    expect(fileIconKind('main.rs')).toBe('code')
    expect(fileIconKind('base.css')).toBe('style')
    expect(fileIconKind('prism.json')).toBe('data')
    expect(fileIconKind('Cargo.lock')).toBe('data')
    expect(fileIconKind('README.md')).toBe('markdown')
    expect(fileIconKind('logo.png')).toBe('image')
    expect(fileIconKind('mark.svg')).toBe('image')
    expect(fileIconKind('release.sh')).toBe('shell')
    expect(fileIconKind('notes.txt')).toBe('text')
  })

  it('大小写不敏感', () => {
    expect(fileIconKind('README.MD')).toBe('markdown')
    expect(fileIconKind('A.TS')).toBe('code')
  })

  it('没有扩展名的约定文件名', () => {
    expect(fileIconKind('Dockerfile')).toBe('shell')
    expect(fileIconKind('Makefile')).toBe('shell')
    expect(fileIconKind('.gitignore')).toBe('data')
  })

  it('认不出来的＝普通文件', () => {
    expect(fileIconKind('app')).toBe('file')
    expect(fileIconKind('.')).toBe('file')
    expect(fileIconKind('thing.weird')).toBe('file')
    expect(fileIconKind('')).toBe('file')
  })

  it('传完整路径也只按文件名判断', () => {
    expect(fileIconKind('src/components/Pane.vue')).toBe('code')
    expect(fileIconKind('docs/README.md')).toBe('markdown')
  })
})

describe('FILE_ICON_PATHS', () => {
  it('每种类型都有画法', () => {
    for (const paths of Object.values(FILE_ICON_PATHS)) {
      expect(paths.length).toBeGreaterThan(0)
      for (const path of paths) expect(path.startsWith('M') || path.startsWith('m')).toBe(true)
    }
  })
})
