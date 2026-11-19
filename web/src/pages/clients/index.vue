<template>
  <div class="page">
    <sl-page-header
      title="客户端与令牌"
      sub-title="创建接入客户端、一次性下发 token，并查看在线状态与 proxy 数量"
    >
      <template #extra>
        <PageActionsBar
          :meta="updatedAt ? `更新于 ${updatedAt}` : '等待采样'"
          @refresh="refresh"
        >
          <sl-button type="primary" size="sm" :disabled="!configPath" @click="openCreate">
            <template #leading><sl-icon name="user-add" :size="15" /></template>
            新增客户端
          </sl-button>
        </PageActionsBar>
      </template>
    </sl-page-header>

    <sl-alert
      v-if="!configPath"
      type="warning"
      title="当前服务未启用在线写入"
      description="server 启动时没有加载配置文件，无法创建或轮换客户端 token。请使用 -c 指定 server.toml / server.yaml 后重启。"
      variant="subtle"
      show-icon
    />

    <sl-panel
      title="已配置的客户端"
      :description="`${rows.length} 个凭据 · 每 5 秒自动刷新${onlineCount ? ` · ${onlineCount} 个在线` : ''}`"
      padding="none"
    >
      <template #actions>
        <span class="band-meta">创建/轮换后写入服务端配置，并在 1 秒后重启生效</span>
      </template>
      <div class="mt-client-meta">
        <span>接入地址</span>
        <code>{{ snapshot?.server_addr || 'tunnel.example.com:7000' }}</code>
        <span class="mt-client-meta-sep">/</span>
        <span>证书名</span>
        <code>{{ snapshot?.server_name || 'tunnel.example.com' }}</code>
      </div>
      <div class="mt-table-host">
        <sl-data-table
          :columns="columns"
          :data="rows"
          row-key="id"
          caption="客户端"
          unit="个"
          empty-text="还没有客户端，点击右上角新增"
          :page-size="20"
          :searchable="false"
          :page-sizes="[20, 50, 100]"
          dense
        >
          <template #cell="{ column, row }">
            <span v-if="column.key === 'id'" class="mt-client-id">{{ row.id }}</span>
            <span v-else-if="column.key === 'status'" class="mt-client-status">
              <sl-tag
                size="sm"
                variant="subtle"
                :class="row.online ? 'mt-client-online' : 'mt-client-offline'"
              >{{ row.online ? '在线' : '离线' }}</sl-tag>
            </span>
            <code v-else-if="column.key === 'token_fingerprint'" class="mt-client-fingerprint">{{ row.token_fingerprint }}</code>
            <span v-else-if="column.key === 'allow_ports'" class="mt-mono-cell">{{ row.allow_ports }}</span>
            <span v-else-if="column.key === 'source'" class="mt-client-source">
              {{ row.source === 'clients' ? 'auth.clients' : 'token_sha256 简写' }}
            </span>
            <template v-else>{{ row[column.key] }}</template>
          </template>
          <template #actions="{ row }">
            <div class="mt-inline-actions">
              <sl-button
                type="ghost"
                size="xs"
                icon-only
                aria-label="轮换 token"
                title="轮换 token"
                :disabled="row.source !== 'clients'"
                @click="openRotate(asClient(row))"
              >
                <sl-icon name="refresh" :size="14" />
              </sl-button>
              <sl-popconfirm
                title="删除这个客户端？"
                :description="`删除 ${row.id} 后，该客户端会在服务重启后失效。`"
                confirm-text="删除"
                type="danger"
                :disabled="!row.can_delete"
                :loading="deleting === row.id"
                @confirm="removeClient(asClient(row))"
              >
                <template #trigger="{ triggerProps }">
                  <sl-button
                    type="ghost"
                    size="xs"
                    icon-only
                    class="mt-danger-action"
                    aria-label="删除客户端"
                    title="删除"
                    :disabled="!row.can_delete"
                    v-bind="triggerProps"
                  >
                    <sl-icon name="trash" :size="14" />
                  </sl-button>
                </template>
              </sl-popconfirm>
            </div>
          </template>
        </sl-data-table>
      </div>
    </sl-panel>

    <ClientFormDialog
      :open="dialogOpen"
      :mode="dialogMode"
      :client="editing"
      :form="form"
      :error="dialogError"
      :saving="saving"
      :result="result"
      @update:open="onDialogOpenChange"
      @confirm="submit"
      @cancel="closeDialog"
      @copy="copyText"
    />
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { SlAlert } from '@sailonix/ui/alert'
import { SlButton } from '@sailonix/ui/button'
import { SlDataTable } from '@sailonix/ui/data-table'
import type { DataTableColumn, DataTableRow } from '@sailonix/ui/data-table'
import { SlIcon } from '@sailonix/icon'
import { SlPageHeader } from '@sailonix/ui/page-header'
import { SlPanel } from '@sailonix/ui/panel'
import { SlPopconfirm } from '@sailonix/ui/popconfirm'
import { SlTag } from '@sailonix/ui/tag'
import PageActionsBar from '../../components/PageActionsBar.vue'
import { apiGet, apiWrite, formatTime } from '../../services/api'
import { notifyError, notifySuccess, notifyWarning } from '../../services/notifications'
import ClientFormDialog from './ClientFormDialog.vue'
import type { ClientRow, ClientsSnapshot, ClientWriteResponse } from './types'

const snapshot = ref<ClientsSnapshot | null>(null)
const updatedAt = ref('')
const dialogOpen = ref(false)
const dialogMode = ref<'create' | 'rotate'>('create')
const editing = ref<ClientRow | null>(null)
const saving = ref(false)
const deleting = ref('')
const dialogError = ref('')
const result = ref<ClientWriteResponse | null>(null)
const form = reactive({
  id: '',
  tenant: 'default',
  serverAddr: '',
  allowPorts: '',
})
let timer: number | undefined

const configPath = computed(() => snapshot.value?.config_path ?? null)
const onlineCount = computed(() => rows.value.filter((row) => row.online).length)

const rows = computed(() =>
  (snapshot.value?.clients ?? []).map((client) => ({
    ...client,
    allow_ports: client.allow_ports ? `${client.allow_ports[0]}–${client.allow_ports[1]}` : '不限制',
    lastText: client.last_connected_ms ? formatTime(client.last_connected_ms) : '—',
    proxiesText: `${client.proxy_count} 个`,
    tenant: client.tenant,
  })),
)

const columns: DataTableColumn[] = [
  { key: 'id', header: '客户端 ID', type: 'text', width: 140 },
  { key: 'tenant', header: '租户', type: 'text', width: 100 },
  { key: 'status', header: '状态', type: 'text', width: 80 },
  { key: 'proxiesText', header: 'Proxy', type: 'text', align: 'end', width: 74 },
  { key: 'token_fingerprint', header: 'Token 指纹', type: 'text', width: 180 },
  { key: 'allow_ports', header: '端口范围', type: 'text', width: 116 },
  { key: 'source', header: '来源', type: 'text', width: 130 },
  { key: 'actions', header: '操作', type: 'actions', width: 100 },
]


async function refresh() {
  try {
    const data = await apiGet<ClientsSnapshot>('/api/v1/clients')
    snapshot.value = data
    updatedAt.value = new Date().toLocaleTimeString('zh-CN', { hour12: false })
    if (!form.serverAddr) form.serverAddr = data.server_addr
  } catch (error) {
    notifyError(`读取客户端失败：${String(error instanceof Error ? error.message : error)}`)
  }
}

function openCreate() {
  dialogMode.value = 'create'
  editing.value = null
  result.value = null
  dialogError.value = ''
  form.id = ''
  form.tenant = 'default'
  form.serverAddr = snapshot.value?.server_addr ?? ''
  form.allowPorts = ''
  dialogOpen.value = true
}

function openRotate(client: ClientRow) {
  if (client.source !== 'clients') {
    notifyWarning('当前是 auth.token_sha256 简写，请先新增一个客户端完成迁移')
    return
  }
  dialogMode.value = 'rotate'
  editing.value = client
  result.value = null
  dialogError.value = ''
  form.id = client.id
  form.tenant = client.tenant
  form.serverAddr = snapshot.value?.server_addr ?? ''
  form.allowPorts = client.allow_ports ? `${client.allow_ports[0]}-${client.allow_ports[1]}` : ''
  dialogOpen.value = true
}

function closeDialog() {
  if (saving.value) return
  dialogOpen.value = false
  result.value = null
}

function onDialogOpenChange(value: boolean) {
  if (!value && saving.value) return
  dialogOpen.value = value
  if (!value) result.value = null
}

function parseAllowPorts(value: string): [number, number] | null | string {
  const text = value.trim()
  if (!text) return null
  const match = text.match(/^(\d{4,5})\s*[-–,]\s*(\d{4,5})$/)
  if (!match) return 'allow_ports 需要写成 1024-65535'
  const start = Number(match[1])
  const end = Number(match[2])
  if (start < 1024 || end > 65535 || start > end) {
    return 'allow_ports 需要满足 1024 ≤ start ≤ end ≤ 65535'
  }
  return [start, end]
}

async function submit() {
  dialogError.value = ''
  const allowPorts = parseAllowPorts(form.allowPorts)
  if (typeof allowPorts === 'string') {
    dialogError.value = allowPorts
    return
  }
  saving.value = true
  try {
    const body: Record<string, unknown> = {
      allow_ports: allowPorts,
      server_addr: form.serverAddr.trim(),
    }
    if (dialogMode.value === 'create') {
      body.id = form.id.trim()
      body.tenant = form.tenant.trim() || 'default'
      const response = await apiWrite<ClientWriteResponse>('POST', '/api/v1/clients', body)
      result.value = response
      notifySuccess('客户端已创建，请立即复制 token 与配置')
    } else if (editing.value) {
      const response = await apiWrite<ClientWriteResponse>(
        'PUT',
        `/api/v1/clients/${encodeURIComponent(editing.value.id)}`,
        body,
      )
      result.value = response
      notifySuccess('token 已轮换，旧 token 会在服务重启后失效')
    }
    form.serverAddr = snapshot.value?.server_addr ?? form.serverAddr
    void refresh()
  } catch (error) {
    dialogError.value = String(error instanceof Error ? error.message : error)
  } finally {
    saving.value = false
  }
}

async function removeClient(client: ClientRow) {
  deleting.value = client.id
  try {
    await apiWrite<ClientWriteResponse>('DELETE', `/api/v1/clients/${encodeURIComponent(client.id)}`)
    notifySuccess(`已删除客户端 ${client.id}`)
    void refresh()
  } catch (error) {
    notifyError(String(error instanceof Error ? error.message : error))
  } finally {
    deleting.value = ''
  }
}

function asClient(row: DataTableRow): ClientRow {
  return row as unknown as ClientRow
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    notifySuccess('已复制到剪贴板')
  } catch {
    notifyWarning('浏览器未授权剪贴板，请手动选中复制')
  }
}

onMounted(() => {
  void refresh()
  timer = window.setInterval(() => void refresh(), 5000)
})

onBeforeUnmount(() => {
  if (timer !== undefined) window.clearInterval(timer)
})
</script>
