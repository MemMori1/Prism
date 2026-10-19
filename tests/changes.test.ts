import { describe, expect, it } from 'vitest'
import {
  buildFileTree,
  emptyTextHint,
  flattenFileTree,
  parentDirs,
  fileKindOf,
  filterProjectFiles,
  isJsonPath,
  joinPath,
  looksLikePath,
  numberedLines,
  splitPath,
  statusLabel,
  statusMark,
} from '../src/changes'

describe('statusMark / statusLabel', () => {
  it('四种状态各有字母与中文', () => {
    expect(statusMark('added')).toBe('A')
    expect(statusMark('modified')).toBe('M')
    expect(statusMark('deleted')).toBe('D')
    expect(statusMark('renamed')).toBe('R')
    expect(statusLabel('added')).toBe('新增')
    expect(statusLabel('deleted')).toBe('删除')
    expect(statusLabel('renamed')).toBe('重命名')
    expect(statusLabel('modified')).toBe('修改')
  })
})

describe('splitPath', () => {
  it('拆出目录与文件名', () => {
    expect(splitPath('src/styles/a.css')).toEqual({ dir: 'src/styles', name: 'a.css' })
  })

  it('根目录文件没有目录段', () => {
    expect(splitPath('README.md')).toEqual({ dir: '', name: 'README.md' })
  })
})

describe('isJsonPath', () => {
  it('只看扩展名，大小写不敏感', () => {
    expect(isJsonPath('a/b/c.json')).toBe(true)
    expect(isJsonPath('a/B/C.JSON')).toBe(true)
    expect(isJsonPath('a/b/c.jsonc')).toBe(false)
    expect(isJsonPath(null)).toBe(false)
  })
})

describe('numberedLines', () => {
  it('行号从 1 开始', () => {
    expect(numberedLines('a\nb')).toEqual([
      { no: 1, text: 'a' },
      { no: 2, text: 'b' },
    ])
  })

  it('末尾换行不多出一条空行', () => {
    expect(numberedLines('a\nb\n')).toHaveLength(2)
  })

  it('中间空行照常算一行', () => {
    expect(numberedLines('a\n\nb')).toEqual([
      { no: 1, text: 'a' },
      { no: 2, text: '' },
      { no: 3, text: 'b' },
    ])
  })

  it('空文本＝没有行', () => {
    expect(numberedLines('')).toEqual([])
  })
})

describe('emptyTextHint', () => {
  it('二进制与截断各有提示，正常文件没有', () => {
    expect(emptyTextHint({ binary: true, truncated: false })).toContain('二进制')
    expect(emptyTextHint({ binary: false, truncated: true })).toContain('太大')
    expect(emptyTextHint({ binary: false, truncated: false })).toBeNull()
    expect(emptyTextHint(null)).toBeNull()
  })
})

describe('filterProjectFiles', () => {
  const files = [
    { path: 'src/api.ts', name: 'api.ts' },
    { path: 'src/components/ChangesPane.vue', name: 'ChangesPane.vue' },
    { path: 'README.md', name: 'README.md' },
  ]

  it('空词＝全部，最多截到 limit', () => {
    expect(filterProjectFiles(files, '').items).toHaveLength(3)
    const limited = filterProjectFiles(files, '', 2)
    expect(limited.items).toHaveLength(2)
    expect(limited.total).toBe(3)
  })

  it('路径与文件名都算命中，大小写不敏感', () => {
    expect(filterProjectFiles(files, 'changespane').items.map(f => f.name)).toEqual([
      'ChangesPane.vue',
    ])
    expect(filterProjectFiles(files, 'SRC/').items).toHaveLength(2)
  })

  it('没命中＝空，但 total 仍是 0', () => {
    const view = filterProjectFiles(files, 'zzz')
    expect(view.items).toEqual([])
    expect(view.total).toBe(0)
  })
})

describe('joinPath', () => {
  it('拼绝对路径，兼容尾斜杠与 ./', () => {
    expect(joinPath('/a/b/', './src/x.ts')).toBe('/a/b/src/x.ts')
    expect(joinPath('/a/b', 'src/x.ts')).toBe('/a/b/src/x.ts')
  })

  it('已经是绝对路径就原样返回', () => {
    expect(joinPath('/a/b', '/etc/hosts')).toBe('/etc/hosts')
  })
})

describe('looksLikePath', () => {
  it('带斜杠的相对 / 绝对路径都算', () => {
    expect(looksLikePath('src/api.ts')).toBe(true)
    expect(looksLikePath('./docs/a.md')).toBe(true)
    expect(looksLikePath('/Users/me/project/src')).toBe(true)
  })

  it('没有斜杠时只认常见扩展名与约定文件名', () => {
    expect(looksLikePath('README.md')).toBe(true)
    expect(looksLikePath('Cargo.lock')).toBe(true)
    expect(looksLikePath('Dockerfile')).toBe(true)
    expect(looksLikePath('v1.2.3')).toBe(false)
    expect(looksLikePath('npm')).toBe(false)
  })

  it('命令、协议、空白、选项都不算', () => {
    expect(looksLikePath('npm run build')).toBe(false)
    expect(looksLikePath('https://example.com/a.ts')).toBe(false)
    expect(looksLikePath('--force')).toBe(false)
    expect(looksLikePath('@scope/pkg')).toBe(false)
    expect(looksLikePath('')).toBe(false)
  })
})

describe('fileKindOf', () => {
  it('markdown / 图片 / 其他', () => {
    expect(fileKindOf('README.md')).toBe('markdown')
    expect(fileKindOf('docs/a.MARKDOWN')).toBe('markdown')
    expect(fileKindOf('assets/logo.png')).toBe('image')
    expect(fileKindOf('icon.svg')).toBe('image')
    expect(fileKindOf('src/api.ts')).toBe('text')
    expect(fileKindOf('Dockerfile')).toBe('text')
    expect(fileKindOf(null)).toBe('text')
  })
})

describe('buildFileTree', () => {
  const files = [
    { path: 'src/api.ts', name: 'api.ts' },
    { path: 'src/components/Pane.vue', name: 'Pane.vue' },
    { path: 'src/main.ts', name: 'main.ts' },
    { path: 'README.md', name: 'README.md' },
    { path: 'docs/a/b.md', name: 'b.md' },
  ]

  it('目录在前、文件在后，中间目录补齐', () => {
    const tree = buildFileTree(files)
    expect(tree.map(node => node.name)).toEqual(['docs', 'src', 'README.md'])
    expect(tree[0].dir).toBe(true)
    expect(tree[0].children.map(node => node.name)).toEqual(['a'])
    expect(tree[1].children.map(node => node.name)).toEqual(['components', 'api.ts', 'main.ts'])
  })

  it('同名目录不会重复出现', () => {
    const tree = buildFileTree(files)
    const src = tree.find(node => node.path === 'src')
    expect(src?.children.filter(node => node.dir)).toHaveLength(1)
  })

  it('同级按名字排（大小写不敏感）', () => {
    const tree = buildFileTree([
      { path: 'b.ts', name: 'b.ts' },
      { path: 'A.ts', name: 'A.ts' },
    ])
    expect(tree.map(node => node.name)).toEqual(['A.ts', 'b.ts'])
  })

  it('空列表＝空树', () => {
    expect(buildFileTree([])).toEqual([])
  })
})

describe('flattenFileTree', () => {
  const files = [
    { path: 'src/api.ts', name: 'api.ts' },
    { path: 'src/components/Pane.vue', name: 'Pane.vue' },
    { path: 'README.md', name: 'README.md' },
  ]

  it('默认全展开，带正确缩进', () => {
    const rows = flattenFileTree(buildFileTree(files), () => true)
    expect(rows.map(row => `${row.depth}:${row.name}`)).toEqual([
      '0:src',
      '1:components',
      '2:Pane.vue',
      '1:api.ts',
      '0:README.md',
    ])
  })

  it('折叠的目录不出子节点，但目录自己还在', () => {
    const rows = flattenFileTree(buildFileTree(files), path => path !== 'src')
    expect(rows.map(row => row.name)).toEqual(['src', 'README.md'])
  })

  it('文件节点带着原始对象', () => {
    const rows = flattenFileTree(buildFileTree(files), () => true)
    const api = rows.find(row => row.name === 'api.ts')
    expect(api?.dir).toBe(false)
    expect(api?.file?.path).toBe('src/api.ts')
  })
})

describe('parentDirs', () => {
  it('列出所有祖先目录（不含文件名）', () => {
    expect(parentDirs('src/components/ui/Pane.vue')).toEqual([
      'src',
      'src/components',
      'src/components/ui',
    ])
  })

  it('根下的文件没有祖先', () => {
    expect(parentDirs('README.md')).toEqual([])
  })
})
