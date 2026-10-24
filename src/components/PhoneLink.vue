<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { onPhoneLinkEvent } from '../phoneLinkEvents'
import { pairingLeftSeconds, qrAvailable as qrAvailableNow, statusLabel as statusLabelOf, statusTone as statusToneOf, webhookPlaceholder as webhookPlaceholderOf } from '../phoneLink'
import { relativeTime } from '../format'
import SelectMenu from './SelectMenu.vue'
import type { PhoneLinkStatus, PhonePushEvents } from '../types'

/**
 * 手机链接：左卡＝LAN 配对（状态 + 二维码 + 引擎），右卡＝微信 Bot 推送。
 * 后端是 phone.link 插件；这里只做状态呈现与增量配置。
 */

const status = ref<PhoneLinkStatus | null>(null)
const loading = ref(true)
const errorText = ref('')
const notice = ref('')

const webhookDraft = ref('')
const savingWebhook = ref(false)
const testingWebhook = ref(false)
const toggling = ref(false)
const unpairing = ref(false)

const ENGINE_OPTIONS = [
  { value: 'opencode', label: 'OpenCode' },
  { value: 'codex', label: 'Codex' },
]

function flash(message: string): void {
  notice.value = message
  window.setTimeout(() => {
    if (notice.value === message) notice.value = ''
  }, 4000)
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}

async function apply(action: () => Promise<PhoneLinkStatus>): Promise<void> {
  errorText.value = ''
  try {
    status.value = await action()
  } catch (cause) {
    errorText.value = errorMessage(cause)
  }
}

async function load(options: { autoPair?: boolean } = {}): Promise<void> {
  errorText.value = ''
  try {
    const next = await api.phoneLinkStatus()
    status.value = next
    // 已启用但还没有有效配对码：生成一轮，让二维码常驻可扫。
    const expired = !next.pairing || next.pairing.expiresAt <= Date.now()
    if (options.autoPair && next.enabled && next.serverRunning && expired) {
      status.value = await api.phoneLinkPairRefresh()
    }
  } catch (cause) {
    errorText.value = errorMessage(cause)
  } finally {
    loading.value = false
  }
}

async function toggle(): Promise<void> {
  if (!status.value || toggling.value) return
  toggling.value = true
  const enabled = !status.value.enabled
  await apply(() => api.phoneLinkEnable(enabled))
  toggling.value = false
  flash(enabled ? '手机链接已启用，扫码配对即可' : '手机链接已停用')
}

async function refreshQr(): Promise<void> {
  await apply(() => api.phoneLinkPairRefresh())
}

async function unpair(): Promise<void> {
  if (!status.value?.device || unpairing.value) return
  unpairing.value = true
  await apply(() => api.phoneLinkUnpair())
  unpairing.value = false
  flash('已解除配对，手机端下次请求会要求重新扫码')
}

async function saveEngine(engine: string): Promise<void> {
  await apply(() => api.phoneLinkConfigSave({ engine }))
  flash('手机会话引擎已更新')
}

async function saveWebhook(): Promise<void> {
  const url = webhookDraft.value.trim()
  if (!url || savingWebhook.value) return
  savingWebhook.value = true
  await apply(() => api.phoneLinkConfigSave({ webhookUrl: url }))
  savingWebhook.value = false
  if (!errorText.value) {
    webhookDraft.value = ''
    flash('Webhook 已保存（原文不再回显）')
  }
}

async function saveEvents(patch: Partial<PhonePushEvents>): Promise<void> {
  if (!status.value) return
  const events: PhonePushEvents = { ...status.value.events, ...patch }
  await apply(() => api.phoneLinkConfigSave({ events }))
}

async function testWebhook(): Promise<void> {
  if (testingWebhook.value) return
  testingWebhook.value = true
  errorText.value = ''
  try {
    await api.phoneLinkWebhookTest()
    flash('测试消息已发送，去群里看看')
  } catch (cause) {
    errorText.value = errorMessage(cause)
  } finally {
    testingWebhook.value = false
  }
}

// ---------- 状态派生（文案 / 色调在 ../phoneLink 纯模块里，可测） ----------

const statusLabel = computed(() => statusLabelOf(status.value))

const statusTone = computed(() => statusToneOf(status.value))

const deviceSeen = computed(() => {
  const device = status.value?.device
  return device ? `${relativeTime(device.lastSeenAt)}活跃` : ''
})

const webhookPlaceholder = computed(() => webhookPlaceholderOf(status.value))

// ---------- 二维码与倒计时 ----------

const nowTick = ref(Date.now())
let ticker: number | null = null

const pairingLeft = computed(() => pairingLeftSeconds(status.value, nowTick.value))

const qrAvailable = computed(() => qrAvailableNow(status.value, nowTick.value))

const qrHint = computed(() => {
  if (!qrAvailable.value) return ''
  const parts = ['手机与电脑需在同一 Wi-Fi']
  if (status.value?.device) parts.push('重新扫码会顶掉旧设备')
  parts.push(`${pairingLeft.value}s 后过期`)
  return parts.join('；')
})

// 过期自动换码：只兜一次，新状态回来前不再连续触发。
let autoRefreshing = false
watch(pairingLeft, left => {
  nowTick.value = Date.now()
  if (left > 0 || autoRefreshing) return
  const value = status.value
  if (!value?.enabled || !value.serverRunning) return
  autoRefreshing = true
  void refreshQr().finally(() => {
    autoRefreshing = false
  })
})

// ---------- 事件订阅 ----------

let stopEvents: (() => void) | null = null

onMounted(() => {
  ticker = window.setInterval(() => {
    nowTick.value = Date.now()
  }, 1000)
  void load({ autoPair: true })
  stopEvents = onPhoneLinkEvent(payload => {
    if (payload.kind === 'paired' || payload.kind === 'unpaired' || payload.kind === 'turn') {
      void load()
    }
  })
})

onBeforeUnmount(() => {
  if (ticker != null) window.clearInterval(ticker)
  stopEvents?.()
  stopEvents = null
})
</script>

<template>
  <div class="pl">
    <p v-if="errorText" class="pl-banner pl-error" role="alert">{{ errorText }}</p>
    <p v-else-if="notice" class="pl-banner pl-ok">{{ notice }}</p>

    <div v-if="loading && !status" class="pl-loading">加载中…</div>
    <div v-else-if="status" class="pl-grid">
      <!-- 左：手机对话（扫码配对） -->
      <section class="pl-card">
        <header class="pl-card-head">
          <span class="pl-card-title">手机对话</span>
          <span class="pl-card-hint">扫码后手机直接指挥桌面 Agent</span>
          <span class="pl-spacer" />
          <button
            class="pl-btn"
            :class="{ 'pl-btn-primary': !status.enabled }"
            :disabled="toggling"
            :title="status.enabled ? '停用 LAN 服务' : '启用并生成配对二维码'"
            @click="toggle"
          >
            {{ toggling ? '…' : status.enabled ? '停用' : '启用' }}
          </button>
        </header>

        <div class="pl-row">
          <span class="pl-row-key">状态</span>
          <span class="pl-row-val">
            <span class="pl-dot" :class="`tone-${statusTone}`" />
            {{ statusLabel }}
            <span v-if="deviceSeen" class="pl-faint">{{ deviceSeen }}</span>
          </span>
        </div>
        <div class="pl-row">
          <span class="pl-row-key">引擎</span>
          <SelectMenu
            :model-value="status.engine"
            :options="ENGINE_OPTIONS"
            size="sm"
            inline
            title="手机会话用的引擎"
            :disabled="!status.enabled"
            @update:model-value="saveEngine"
          />
        </div>

        <div class="pl-qr-area">
          <div v-if="qrAvailable" class="pl-qr" v-html="status.qrSvg" />
          <div v-else class="pl-qr-empty">
            <template v-if="!status.enabled">
              启用后这里会出现二维码；<br>用微信「扫一扫」或相机扫码即可配对。
            </template>
            <template v-else-if="!status.serverRunning">
              服务没有起来（端口被占？）——<br>停用再启用一次即可重试。
            </template>
            <template v-else-if="status.device">
              已有设备配对（{{ status.device.name }}）。<br>换设备 / 重新扫码会顶掉旧的。
            </template>
            <template v-else>正在生成二维码…</template>
          </div>
          <p v-if="qrAvailable" class="pl-qr-hint">{{ qrHint }}</p>
        </div>

        <div class="pl-acts">
          <button
            v-if="status.enabled && status.serverRunning"
            class="pl-btn"
            :disabled="!status.pairing && !status.device"
            title="作废当前配对码并生成新的"
            @click="refreshQr"
          >
            刷新二维码
          </button>
          <button
            v-if="status.device"
            class="pl-btn"
            :disabled="unpairing"
            title="设备令牌作废，手机端下次请求需重新扫码"
            @click="unpair"
          >
            {{ unpairing ? '…' : '解除配对' }}
          </button>
        </div>
        <p class="pl-note">
          会话工作区固定在 <code>~/.prism/phone-workspace</code>，不碰你的项目目录；
          手机页只在本机局域网可达，停用即关闭。
        </p>
      </section>

      <!-- 右：微信 Bot（扫码对话为主，Webhook 推送可选） -->
      <section class="pl-card">
        <header class="pl-card-head">
          <span class="pl-card-title">微信 Bot</span>
          <span class="pl-card-hint">微信扫一扫，对话页开在微信里</span>
        </header>

        <div class="pl-row">
          <span class="pl-row-key">状态</span>
          <span class="pl-row-val">
            <span class="pl-dot" :class="status.enabled ? 'tone-ok' : 'tone-muted'" />
            {{ status.enabled ? '扫码对话可用' : '未启用' }}
          </span>
        </div>
        <p class="pl-note">
          个人微信没有官方机器人接口——「在微信聊天窗口里跟 bot 对话」只能走非官方协议
          （注入客户端、有封号风险），Prism 不内置。用微信「扫一扫」左侧二维码，
          对话页直接开在微信里，这是普通微信的官方打开方式。
        </p>

        <div class="pl-field">
          <span class="pl-label">任务通知（可选 Webhook）</span>
          <input
            v-model="webhookDraft"
            class="pl-input mono"
            type="password"
            autocomplete="off"
            spellcheck="false"
            :placeholder="webhookPlaceholder"
          >
        </div>
        <p class="pl-note">
          兼容「群机器人」消息格式（如企业微信群机器人地址）。任务完成 / 权限请求 /
          出错会推到群里；不需要就留空。保存后原文不回显，只留尾 4 位核对。
        </p>

        <div class="pl-field">
          <span class="pl-label">推送事件</span>
          <div class="pl-chips">
            <button
              class="pl-chip"
              :class="{ on: status.events.onDone }"
              @click="saveEvents({ onDone: !status.events.onDone })"
            >
              任务完成
            </button>
            <button
              class="pl-chip"
              :class="{ on: status.events.onPermission }"
              @click="saveEvents({ onPermission: !status.events.onPermission })"
            >
              权限请求
            </button>
            <button
              class="pl-chip"
              :class="{ on: status.events.onError }"
              @click="saveEvents({ onError: !status.events.onError })"
            >
              运行出错
            </button>
          </div>
        </div>

        <div class="pl-acts">
          <button
            class="pl-btn"
            :disabled="!status.webhookConfigured || testingWebhook"
            title="发一条测试消息到 webhook"
            @click="testWebhook"
          >
            {{ testingWebhook ? '发送中…' : '发送测试消息' }}
          </button>
          <button
            class="pl-btn pl-btn-primary"
            :disabled="!webhookDraft.trim() || savingWebhook"
            @click="saveWebhook"
          >
            {{ savingWebhook ? '保存中…' : '保存 Webhook' }}
          </button>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped src="../styles/components/PhoneLink.css"></style>
