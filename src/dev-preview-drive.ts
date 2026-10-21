/**
 * 预览自驱脚本：`?drive=flow` 时自己把关键交互点一遍，把每一步的探针结果写进
 * `<pre id="probe">`——headless Chrome 用 `--dump-dom` 读它。
 *
 * 为什么自驱：截图只能证明"没白屏"，点一遍才能证明"点得动"。不依赖任何浏览器
 * 自动化工具，CI 里也能跑。
 */
const params = new URLSearchParams(location.search)

function sleep(ms: number) {
  return new Promise(resolve => window.setTimeout(resolve, ms))
}

function text(selector: string): string | null {
  return document.querySelector(selector)?.textContent?.trim() ?? null
}

function texts(selector: string): string[] {
  return [...document.querySelectorAll(selector)].map(el => el.textContent?.trim() ?? '')
}

async function click(selector: string, index = 0) {
  const el = document.querySelectorAll(selector)[index] as HTMLElement | undefined
  if (!el) throw new Error(`找不到 ${selector}[${index}]`)
  el.click()
  await sleep(350)
}

async function type(selector: string, value: string) {
  const el = document.querySelector(selector) as HTMLTextAreaElement | HTMLInputElement | null
  if (!el) throw new Error(`找不到 ${selector}`)
  el.focus()
  el.value = value
  el.dispatchEvent(new Event('input', { bubbles: true }))
  await sleep(80)
}

function probe() {
  const counts = (window as unknown as { __calls?: Record<string, number> }).__calls ?? {}
  const workspace = localStorage.getItem('prism-workspace-v1')
  return {
    navTitle: text('.nav-title'),
    tabs: texts('.nav-tab'),
    leftTitles: texts('.wb-start-item-title'),
    picker: texts('.engine-row[aria-label]').length
      ? [...document.querySelectorAll('.engine-row')].map(el => el.getAttribute('aria-label'))
      : null,
    draft: text('.draft-pane h2'),
    panelOpen: !!document.querySelector('.nav-panel'),
    navPages: texts('.nav-page-label'),
    navRecent: texts('.nav-item-title'),
    chatTitle: text('.session-head .session-title') ?? text('.wb-head .wb-title'),
    calls: {
      session_create: counts.session_create ?? 0,
      session_send: counts.session_send ?? 0,
      session_summary: counts.session_summary ?? 0,
    },
    workspace: workspace ? JSON.parse(workspace) : null,
  }
}

/** ?drive=flow：fresh 场景 → 挑 Agent → 发第一条 → ＋ 新页面 → 切回上一页。 */
async function runFlow() {
  const results: Record<string, unknown> = {}
  results['0-初始（空的新页面）'] = probe()

  // ① 在空页面里挑 OpenCode：页面绑给它，顶栏变成「OpenCode 的会话」。
  await click('.engine-row[aria-label="OpenCode"]')
  await sleep(400)
  results['1-挑 OpenCode 之后'] = probe()

  // ② 顶栏面板：页面列表 + 只有 OpenCode 的最近会话（native 的不出现）。
  await click('.nav-btn')
  await sleep(300)
  results['2-面板打开'] = probe()
  await click('.nav-backdrop')
  await sleep(200)

  // ③ 草稿发出第一条消息：这才真的建会话。
  await type('.draft-input', '先把 README 的安装步骤补全')
  await click('.draft-pane .send-btn')
  await sleep(900)
  results['3-发出第一条消息'] = probe()

  // ④ 顶栏「＋」：新开一个空白页面（重新挑 Agent）。
  await click('.nav-new')
  await sleep(500)
  results['4-新建页面'] = probe()

  // ⑤ 面板里切回上一页。
  await click('.nav-btn')
  await sleep(300)
  const labels = texts('.nav-page-label')
  results['5-面板里的页面'] = { navPages: labels, navRecent: texts('.nav-item-title') }
  const opencodeIndex = labels.findIndex(label => label.includes('OpenCode'))
  if (opencodeIndex >= 0) {
    await click('.nav-page-btn', opencodeIndex)
    await sleep(400)
  }
  results['6-切回 OpenCode 页'] = probe()

  return results
}

/** ?drive=legacy：旧版单画布布局 → 一列一页 + 认领回 native。 */
async function runLegacy() {
  const results: Record<string, unknown> = {}
  await sleep(600)
  results['0-迁移后'] = probe()
  await click('.nav-btn')
  await sleep(300)
  results['1-面板'] = probe()
  return results
}

async function main() {
  const drive = params.get('drive')
  if (!drive) return
  const results = drive === 'legacy' ? await runLegacy() : await runFlow()
  const pre = document.createElement('pre')
  pre.id = 'probe'
  pre.style.cssText = 'position:fixed;left:0;top:0;z-index:9999;max-width:100%;white-space:pre-wrap'
  pre.textContent = JSON.stringify(results, null, 1)
  document.body.appendChild(pre)
  ;(window as unknown as { __probe?: unknown }).__probe = results
}

setTimeout(() => {
  void main()
}, 700)
