<template>
  <div class="page page--audit">
    <sl-page-header
      title="审计日志"
      sub-title="管理操作与客户端生命周期记录，JSONL 按天落盘，可查询最近 90 天"
    >
      <template #extra>
        <PageActionsBar
          :meta="updatedAt ? `更新于 ${updatedAt}` : '等待采样'"
          @refresh="refresh"
        >
          <sl-button type="ghost" size="sm" :disabled="!events.length" @click="exportEvents">
            <template #leading><sl-icon name="download" :size="15" /></template>
            导出当前结果
          </sl-button>
        </PageActionsBar>
      </template>
    </sl-page-header>

    <sl-alert
      v-if="snapshot && !snapshot.persistent"
      type="warning"
      title="审计未落盘"
      description="服务端没有配置文件路径，审计事件只保留在内存；重启后清空。使用 -c 指定 server.toml / server.yaml 后重启即可写入 audit/ 目录。"
      variant="subtle"
      show-icon
    />

    <sl-panel title="过滤条件" description="服务端过滤，最多返回 500 条">
      <div class="mt-audit-filters">
        <sl-form-item label="时间范围">
          <sl-select v-model:value="days" :options="dayOptions" />
        </sl-form-item>
        <sl-form-item label="动作">
          <sl-select v-model:value="action" :options="actionOptions" />
        </sl-form-item>
        <sl-form-item label="结果">
          <sl-select v-model:value="result" :options="resultOptions" />
        </sl-form-item>
        <sl-form-item label="客户端">
          <sl-input v-model:value="client" placeholder="client_id 或目标" clearable />
        </sl-form-item>
        <sl-form-item label="操作者">
          <sl-input v-model:value="actor" placeholder="admin / client:home-nas" clearable />
        </sl-form-item>
      </div>
    </sl-panel>

    <div class="band-label">
      <sl-section-label class="mt-section-label" label="审计明细" />
    </div>

    <sl-panel :description="`${events.length} 条${snapshot?.dir ? ` · ${snapshot.dir}` : ''}`" padding="none">
      <sl-data-table
        :columns="columns"
        :data="rows"
        row-key="trace_id"
        caption="审计事件"
        unit="条"
        empty-text="当前条件下没有审计事件"
        :page-size="50"
        :searchable="false"
        :page-sizes="[50, 100, 200]"
        dense
      >
        <template #cell="{ column, row }">
          <span v-if="column.key === 'time'" class="mt-mono-cell">{{ row.timeText }}</span>
          <span v-else-if="column.key === 'actor'" class="mt-audit-actor">{{ row.actor }}</span>
          <code v-else-if="column.key === 'source_ip'" class="mt-mono-cell">{{ row.source_ip || '—' }}</code>
          <span v-else-if="column.key === 'action'" class="mt-audit-action">{{ row.action }}</span>
          <span v-else-if="column.key === 'target'" class="mt-audit-target">{{ row.target }}</span>
          <span v-else-if="column.key === 'result'" class="mt-client-status">
            <sl-tag
              size="sm"
              variant="subtle"
              :class="row.result === 'ok' ? 'mt-audit-ok' : row.result === 'denied' ? 'mt-audit-denied' : 'mt-audit-error'"
            >{{ row.result }}</sl-tag>
          </span>
          <code v-else-if="column.key === 'trace_id'" class="mt-audit-trace">{{ row.trace_id }}</code>
          <template v-else>{{ row[column.key] }}</template>
        </template>
      </sl-data-table>
    </sl-panel>
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { SlAlert } from '@sailonix/ui/alert'
import { SlButton } from '@sailonix/ui/button'
import { SlDataTable } from '@sailonix/ui/data-table'
import type { DataTableColumn } from '@sailonix/ui/data-table'
import { SlFormItem } from '@sailonix/ui/form'
import { SlIcon } from '@sailonix/icon'
import { SlInput } from '@sailonix/ui/input'
import { SlPageHeader } from '@sailonix/ui/page-header'
import { SlPanel } from '@sailonix/ui/panel'
import { SlSectionLabel } from '@sailonix/ui/section-label'
import { SlSelect } from '@sailonix/ui/select'
import { SlTag } from '@sailonix/ui/tag'
import PageActionsBar from '../../components/PageActionsBar.vue'
import { apiGet, formatTime } from '../../services/api'
import { notifyError } from '../../services/notifications'
import type { AuditEvent, AuditSnapshot } from './types'

const snapshot = ref<AuditSnapshot | null>(null)
const events = ref<AuditEvent[]>([])
const updatedAt = ref('')
const days = ref('7')
const action = ref('')
const result = ref('')
const client = ref('')
const actor = ref('')
let timer: number | undefined

const dayOptions = [
  { label: '最近 24 小时', value: '1' },
  { label: '最近 7 天', value: '7' },
  { label: '最近 30 天', value: '30' },
  { label: '最近 90 天', value: '90' },
]

const actionOptions = [
  { label: '全部动作', value: '' },
  { label: '登录', value: 'login' },
  { label: '登出', value: 'logout' },
  { label: '客户端创建', value: 'client.create' },
  { label: '客户端轮换', value: 'client.rotate' },
  { label: '客户端删除', value: 'client.delete' },
  { label: '客户端上线', value: 'client.online' },
  { label: '客户端下线', value: 'client.offline' },
  { label: 'Proxy 创建', value: 'proxy.create' },
  { label: 'Proxy 更新', value: 'proxy.update' },
  { label: 'Proxy 删除', value: 'proxy.delete' },
  { label: 'Proxy 注销', value: 'proxy.unregister' },
  { label: '设置保存', value: 'settings.update' },
  { label: '配置重载', value: 'config.reload' },
]

const resultOptions = [
  { label: '全部结果', value: '' },
  { label: '成功', value: 'ok' },
  { label: '拒绝', value: 'denied' },
  { label: '失败', value: 'error' },
]

const rows = computed(() =>
  events.value.map((event) => ({
    ...event,
    timeText: formatTime(event.ts_ms),
  })),
)

const columns: DataTableColumn[] = [
  { key: 'time', header: '时间', type: 'text', width: 160 },
  { key: 'actor', header: '操作者', type: 'text', width: 150 },
  { key: 'source_ip', header: '来源 IP', type: 'text', width: 128 },
  { key: 'action', header: '动作', type: 'text', width: 140 },
  { key: 'target', header: '目标', type: 'text', width: 150 },
  { key: 'client_id', header: '客户端', type: 'text', width: 120 },
  { key: 'result', header: '结果', type: 'text', width: 82 },
  { key: 'trace_id', header: 'Trace', type: 'text', width: 140 },
]

async function refresh() {
  const params = new URLSearchParams({
    days: days.value,
    limit: '300',
  })
  if (action.value) params.set('action', action.value)
  if (result.value) params.set('result', result.value)
  if (client.value.trim()) params.set('client', client.value.trim())
  if (actor.value.trim()) params.set('actor', actor.value.trim())
  try {
    const data = await apiGet<AuditSnapshot>(`/api/v1/audit?${params.toString()}`)
    snapshot.value = data
    events.value = data.events
    updatedAt.value = new Date().toLocaleTimeString('zh-CN', { hour12: false })
  } catch (error) {
    notifyError(`读取审计失败：${String(error instanceof Error ? error.message : error)}`)
  }
}

function exportEvents() {
  const lines = events.value.map((event) => JSON.stringify(event)).join('\n')
  const blob = new Blob([lines + '\n'], { type: 'application/x-ndjson' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = `meatus-audit-${new Date().toISOString().slice(0, 10)}.jsonl`
  anchor.click()
  URL.revokeObjectURL(url)
}

onMounted(() => {
  void refresh()
  timer = window.setInterval(() => void refresh(), 15_000)
})

onBeforeUnmount(() => {
  if (timer !== undefined) window.clearInterval(timer)
})
</script>
