#!/usr/bin/env node
/**
 * 生成 macOS 菜单栏（托盘）模板图标：单色 + Alpha，44×44（22pt @2x）。
 *
 * 图形 = Prism 品牌三角轮廓（与 App.vue 内联 SVG 同一路径，viewBox 64）：
 *   M22 14 L42 50 L2 50 Z，stroke 4 —— 等比缩放到 44 画布，
 *   4× 超采样抗锯齿。只用 Node 内置模块（zlib + 手写 PNG 封装），可复跑：
 *
 *   node src-tauri/icons/tray-icon.cjs src-tauri/icons/tray.png
 */
const zlib = require('node:zlib')
const fs = require('node:fs')

const SIZE = 44
const SS = 4 // 每像素 4×4 超采样
const VIEW = 64 // 品牌 SVG 的 viewBox
const scale = SIZE / VIEW
const points = [
  [22, 14],
  [42, 50],
  [2, 50],
].map(([x, y]) => [x * scale, y * scale])
const half = (4 * scale) / 2

function distToSegment(px, py, ax, ay, bx, by) {
  const vx = bx - ax
  const vy = by - ay
  const wx = px - ax
  const wy = py - ay
  const c1 = vx * wx + vy * wy
  if (c1 <= 0) return Math.hypot(px - ax, py - ay)
  const c2 = vx * vx + vy * vy
  if (c2 <= c1) return Math.hypot(px - bx, py - by)
  const t = c1 / c2
  return Math.hypot(px - (ax + t * vx), py - (ay + t * vy))
}

const rgba = Buffer.alloc(SIZE * SIZE * 4)
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    let cover = 0
    for (let sy = 0; sy < SS; sy++) {
      for (let sx = 0; sx < SS; sx++) {
        const px = x + (sx + 0.5) / SS
        const py = y + (sy + 0.5) / SS
        let inside = false
        for (let i = 0; i < 3; i++) {
          const [ax, ay] = points[i]
          const [bx, by] = points[(i + 1) % 3]
          if (distToSegment(px, py, ax, ay, bx, by) <= half) {
            inside = true
            break
          }
        }
        if (inside) cover++
      }
    }
    const offset = (y * SIZE + x) * 4
    rgba[offset] = 0
    rgba[offset + 1] = 0
    rgba[offset + 2] = 0
    rgba[offset + 3] = Math.round((255 * cover) / (SS * SS))
  }
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c >>> 0
  }
  return table
})()

function crc32(buf) {
  let c = 0xffffffff
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

function chunk(type, data) {
  const length = Buffer.alloc(4)
  length.writeUInt32BE(data.length, 0)
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(body), 0)
  return Buffer.concat([length, body, crc])
}

const ihdr = Buffer.alloc(13)
ihdr.writeUInt32BE(SIZE, 0)
ihdr.writeUInt32BE(SIZE, 4)
ihdr[8] = 8 // 位深
ihdr[9] = 6 // 颜色类型：RGBA
const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE)
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0 // 过滤器：None
  rgba.copy(raw, y * (SIZE * 4 + 1) + 1, y * SIZE * 4, (y + 1) * SIZE * 4)
}
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk('IHDR', ihdr),
  chunk('IDAT', zlib.deflateSync(raw)),
  chunk('IEND', Buffer.alloc(0)),
])

const target = process.argv[2] || 'tray.png'
fs.writeFileSync(target, png)
console.log(`已生成 ${target}（${SIZE}×${SIZE}，模板单色）`)
