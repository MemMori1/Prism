<script setup lang="ts">
import { computed } from 'vue'
import { NAV_ITEMS, type AppView } from '../nav'
import type { WorkbenchStatus } from '../trayStatus'

const props = defineProps<{
  view: AppView
  wbStatus: WorkbenchStatus
}>()

const emit = defineEmits<{
  (event: 'select', view: AppView): void
  (event: 'settings'): void
}>()

/** 工作台角标：等待审批优先显示数量，其次用绿点表示执行中。 */
const waitingBadge = computed(() => {
  const count = props.wbStatus.waiting
  if (count <= 0) return null
  return count > 9 ? '9+' : String(count)
})

const SETTINGS_ICON = ['M4 7h16', 'M4 12h16', 'M4 17h16', 'M9 5.5v3', 'M15 10.5v3', 'M8 15.5v3']
</script>

<template>
  <nav class="rail" aria-label="主导航">
    <div class="rail-group">
      <button
        v-for="item in NAV_ITEMS"
        :key="item.view"
        class="rail-item"
        :class="{ active: view === item.view }"
        :title="item.hint"
        :aria-label="item.hint"
        :aria-current="view === item.view ? 'page' : undefined"
        @click="emit('select', item.view)"
      >
        <svg
          class="rail-icon"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.7"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path v-for="d in item.icon" :key="d" :d="d" />
        </svg>
        <span v-if="item.view === 'workbench' && waitingBadge" class="rail-badge">
          {{ waitingBadge }}
        </span>
        <span
          v-else-if="item.view === 'workbench' && wbStatus.running"
          class="rail-dot"
          :title="`${wbStatus.running} 个会话执行中`"
        />
      </button>
    </div>

    <div class="rail-spacer" />

    <button class="rail-item" title="设置" aria-label="设置" @click="emit('settings')">
      <svg
        class="rail-icon"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.7"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path v-for="d in SETTINGS_ICON" :key="d" :d="d" />
      </svg>
    </button>
  </nav>
</template>

<style scoped>
.rail {
  display: flex;
  flex: none;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  width: 48px;
  padding: 6px 0 10px;
  border-right: 1px solid var(--border);
  background: var(--panel-2);
}

.rail-group {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.rail-spacer {
  flex: 1;
}

.rail-item {
  position: relative;
  display: grid;
  place-items: center;
  width: 34px;
  height: 34px;
  border-radius: var(--radius-sm);
  color: var(--muted);
  transition:
    color 0.15s ease,
    background 0.15s ease;
}

.rail-item:hover {
  color: var(--text);
  background: color-mix(in srgb, var(--border) 45%, transparent);
}

/* 选中态参考 monet：纸片底 + 左侧强调条。 */
.rail-item.active {
  background: var(--accent-soft);
  color: var(--accent-text);
  box-shadow: 0 1px 2px rgb(18 16 34 / 0.06);
}

.rail-item.active::before {
  content: '';
  position: absolute;
  top: 8px;
  bottom: 8px;
  left: -7px;
  width: 2px;
  border-radius: 1px;
  background: var(--accent);
}

.rail-icon {
  width: 18px;
  height: 18px;
}

.rail-badge {
  position: absolute;
  top: 1px;
  right: 1px;
  min-width: 13px;
  padding: 0 3px;
  border-radius: 4px;
  background: var(--amber);
  color: #fff;
  font-size: 9px;
  font-weight: 700;
  line-height: 13px;
  text-align: center;
}

.rail-dot {
  position: absolute;
  right: 3px;
  bottom: 3px;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--green);
  animation: rail-pulse 1.6s ease infinite;
}

@keyframes rail-pulse {
  50% {
    opacity: 0.35;
  }
}
</style>
