/**
 * 复制纯文本。
 *
 * 正常走 Clipboard API；万一不可用（老 webview / 权限被拒），退回
 * 「临时 textarea + execCommand」这条老路——复制是次要动作，不能反过来把界面搞崩。
 */
export async function copyPlainText(text: string): Promise<boolean> {
  if (!text) return false
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    const area = document.createElement('textarea')
    area.value = text
    area.style.position = 'fixed'
    area.style.top = '0'
    area.style.opacity = '0'
    document.body.append(area)
    area.select()
    try {
      return document.execCommand('copy')
    } catch {
      return false
    } finally {
      area.remove()
    }
  }
}
