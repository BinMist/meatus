<template>
  <sl-panel
    class="mt-panel-flush"
    title="server 日志"
    :description="`共 ${total} 行 · 显示 ${entries.length} 行 · 每 2 秒拉取一次`"
    padding="none"
  >
    <template #actions>
      <span class="band-meta">{{ updatedAt ? `更新于 ${updatedAt}` : '等待采样' }}</span>
    </template>
    <sl-empty-state
      v-if="!entries.length"
      class="mt-log-empty"
      :title="sourceCount ? '当前级别没有日志' : '暂无日志'"
      :description="
        sourceCount
          ? '切换级别或点击「全部」查看完整缓冲。'
          : 'server 启动后产生的 tracing 事件会出现在这里。'
      "
      size="sm"
    />
    <div v-else ref="scrollRef" class="mt-log-scroll" role="log" aria-label="server 日志">
      <LogLine v-for="entry in entries" :key="entry.no" :entry="entry" />
    </div>
  </sl-panel>
</template>

<script setup lang="ts">
import { nextTick, onMounted, ref, watch } from 'vue'
import { SlEmptyState } from '@sailonix/ui/empty-state'
import { SlPanel } from '@sailonix/ui/panel'
import LogLine from './LogLine.vue'

defineOptions({ name: 'LogsPanel' })

const props = defineProps<{
  entries: Array<{
    no: number
    time: string
    level: string
    target: string
    message: string
  }>
  total: number
  sourceCount: number
  updatedAt: string
  follow: boolean
}>()

const scrollRef = ref<HTMLElement | null>(null)

function scrollToBottom() {
  nextTick(() => {
    if (scrollRef.value) scrollRef.value.scrollTop = scrollRef.value.scrollHeight
  })
}

watch([() => props.entries, () => props.follow], () => {
  if (props.follow) scrollToBottom()
})
onMounted(scrollToBottom)
</script>
