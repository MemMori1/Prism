<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { EngineHealth, ProjectItem } from '../types'
import { basename } from '../format'
import { engineChoices } from '../sessionStart'
import EngineChoicePanel from './EngineChoicePanel.vue'

const props = withDefaults(
  defineProps<{
    project: ProjectItem | null
    engines: EngineHealth[]
    projects?: ProjectItem[]
    busy?: boolean
    error?: string | null
    cancelable?: boolean
  }>(),
  {
    projects: () => [],
    busy: false,
    error: null,
    cancelable: true,
  },
)

const emit = defineEmits<{
  (e: 'select', engine: string): void
  (e: 'project', projectId: string): void
  (e: 'cancel'): void
}>()

const choosing = ref<string | null>(null)
const choices = computed(() => engineChoices(props.engines))
const projectName = computed(() =>
  props.project ? props.project.name || basename(props.project.worktree) : '',
)

watch(
  () => props.busy,
  busy => {
    if (!busy) choosing.value = null
  },
)

watch(
  () => props.error,
  error => {
    if (error) choosing.value = null
  },
)

function selectEngine(engine: string) {
  if (props.busy || choosing.value) return
  choosing.value = engine
  emit('select', engine)
}

function pickProject(event: Event) {
  emit('project', (event.target as HTMLSelectElement).value)
}
</script>

<template>
  <section class="new-session">
    <header class="new-session-head">
      <select
        v-if="projects.length > 1"
        class="project-select"
        :value="project?.id ?? ''"
        title="选择工作目录"
        @change="pickProject"
      >
        <option v-if="!project" value="" disabled>选择项目</option>
        <option v-for="item in projects" :key="item.id" :value="item.id">
          {{ item.name || basename(item.worktree) }}
        </option>
      </select>
      <span v-else-if="project" class="project-chip" :title="project.worktree">
        <span class="project-dot" />
        {{ projectName }}
      </span>
      <span class="head-title">新会话</span>
      <span class="head-spacer" />
      <button v-if="cancelable" class="head-close" title="关闭" @click="emit('cancel')">✕</button>
    </header>

    <EngineChoicePanel
      v-if="project"
      :choices="choices"
      :selecting="choosing"
      :error="error"
      :hint="busy ? '正在创建空白会话…' : '创建后可直接在会话里输入第一条消息'"
      @select="selectEngine"
    />
    <div v-else class="new-session-empty">还没有可用的工作目录，请先在归档里打开一个项目</div>
  </section>
</template>

<style scoped src="../styles/components/NewSessionPanel.css"></style>
