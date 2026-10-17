import { describe, expect, it } from 'vitest'
import {
  applyCompletion,
  commandInvocation,
  detectTrigger,
  fileLabel,
  shellCommand,
} from '../src/composer'

describe('detectTrigger', () => {
  it('行首或空白后的 @ 触发文件补全', () => {
    expect(detectTrigger('@src', 4)).toEqual({ kind: 'file', start: 0, term: 'src' })
    expect(detectTrigger('请读 @src/co', 10)).toEqual({ kind: 'file', start: 3, term: 'src/co' })
    expect(detectTrigger('第一行\n@a', 6)).toEqual({ kind: 'file', start: 4, term: 'a' })
  })

  it('邮箱样式的 @ 不触发', () => {
    expect(detectTrigger('a@b', 3)).toBeNull()
  })

  it('@ 后带空格视为普通文本', () => {
    expect(detectTrigger('@src/my file', 12)).toBeNull()
  })

  it('行首 / 触发命令补全（含前导空白）', () => {
    expect(detectTrigger('/rev', 4)).toEqual({ kind: 'command', start: 0, term: 'rev' })
    expect(detectTrigger('  /init', 7)).toEqual({ kind: 'command', start: 2, term: 'init' })
    expect(detectTrigger('/init 参数', 9)).toBeNull()
  })

  it('行中间的 / 不触发', () => {
    expect(detectTrigger('x /y', 4)).toBeNull()
  })

  it('越界光标返回 null', () => {
    expect(detectTrigger('abc', 99)).toBeNull()
    expect(detectTrigger('abc', -1)).toBeNull()
  })
})

describe('applyCompletion', () => {
  it('替换触发词并原样保留光标后的文本', () => {
    const text = '看下 @src/co 再说'
    const trigger = detectTrigger(text, 10) // @ 后到“co”之后
    expect(trigger).not.toBeNull()
    const result = applyCompletion(text, trigger!, '@src/components/', 10)
    expect(result.text).toBe('看下 @src/components/  再说')
    expect(result.caret).toBe('看下 @src/components/ '.length)
  })

  it('命令补全插到行首位置', () => {
    const text = '  /rev'
    const trigger = detectTrigger(text, 6)!
    const result = applyCompletion(text, trigger, '/review（审查变更）', 6)
    expect(result.text).toBe('  /review（审查变更） ')
  })
})

describe('shellCommand', () => {
  it('! 开头返回命令本体', () => {
    expect(shellCommand('!ls -la')).toBe('ls -la')
    expect(shellCommand('  !git status  ')).toBe('git status')
    expect(shellCommand('!!')).toBe('!')
  })

  it('只有感叹号返回空串（调用方应视为无效）', () => {
    expect(shellCommand('!')).toBe('')
    expect(shellCommand('!   ')).toBe('')
  })

  it('非 shell 草稿返回 null', () => {
    expect(shellCommand('hello')).toBeNull()
    expect(shellCommand('ls -la')).toBeNull()
  })
})

describe('commandInvocation', () => {
  const commands = [{ name: 'init' }, { name: 'review' }]

  it('已知命令 + 参数可解析', () => {
    expect(commandInvocation('/init 初始化', commands)).toEqual({ name: 'init', text: '初始化' })
    expect(commandInvocation('/review', commands)).toEqual({ name: 'review', text: '' })
    expect(commandInvocation('  /init  x  ', commands)).toEqual({ name: 'init', text: 'x' })
  })

  it('未知命令不算调用（按普通文本发送）', () => {
    expect(commandInvocation('/unknown x', commands)).toBeNull()
    expect(commandInvocation('普通 /init', commands)).toBeNull()
  })
})

describe('fileLabel', () => {
  it('取末段', () => {
    expect(fileLabel('src/components/App.vue')).toBe('App.vue')
    expect(fileLabel('README.md')).toBe('README.md')
  })
})
