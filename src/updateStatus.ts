/**
 * 更新流程的纯函数：把 updater 插件的原始错误转成用户可读短句。
 */

/** 把 updater 插件抛出的原始错误文本转为界面可展示的短句。 */
export function friendlyUpdateError(raw: string): string {
  const text = raw.trim()
  if (text.length === 0) return '未知错误'
  if (/valid release|404/i.test(text)) return '发布源暂无版本（首次发布后可用）'
  if (/signature/i.test(text)) return '签名校验失败（产物与公钥不匹配）'
  if (/error sending request|trying to connect|dns|network|timed out|timeout/i.test(text)) {
    return '网络不可达，稍后再试'
  }
  return text.length > 120 ? `${text.slice(0, 120)}…` : text
}
