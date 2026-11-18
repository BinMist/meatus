<template>
  <div class="page">
    <sl-page-header
      title="Proxy 管理"
      sub-title="通过 admin API 增删改运行中的反向代理 · 写操作经控制帧由客户端确认"
    >
      <template #extra>
        <PageActionsBar
          :meta="updatedAt ? `更新于 ${updatedAt}` : '等待采样'"
          @refresh="refresh"
        >
          <sl-button type="primary" size="sm" :disabled="!clients.length" @click="openCreate">
            <template #leading><sl-icon name="plus" :size="15" /></template>
            新增 proxy
          </sl-button>
        </PageActionsBar>
      </template>
    </sl-page-header>

    <ProxyOverview :stats="summaryStats" :proxy-count="proxies.length" :client-count="clients.length" />

    <ProxySearchBar
      :keyword="search.keyword"
      :type="search.type"
      @update:keyword="search.keyword = $event"
      @update:type="search.type = $event"
      @submit="applySearch"
      @reset="resetSearch"
    />

    <ProxyListPanel
      :rows="rows"
      :deleting="deleting"
      @copy="copyAddress"
      @edit="openEdit"
      @remove="removeProxy"
    />

    <ProxyFormDialog
      :open="dialogOpen"
      :editing="editing"
      :form="form"
      :client-options="clientOptions"
      :error="dialogError"
      :saving="saving"
      @update:open="onDialogOpenChange"
      @confirm="submit"
      @cancel="resetDialog"
    />
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { SlButton } from '@sailonix/ui/button'
import { SlIcon } from '@sailonix/icon'
import { SlPageHeader } from '@sailonix/ui/page-header'
import PageActionsBar from '../../components/PageActionsBar.vue'
import ProxyFormDialog from './components/ProxyFormDialog.vue'
import ProxyListPanel from './components/ProxyListPanel.vue'
import ProxyOverview from './components/ProxyOverview.vue'
import ProxySearchBar from './components/ProxySearchBar.vue'
import { dismissNotification, notifyError, notifySuccess, notifyWarning } from '../../services/notifications'
import { apiGet, apiWrite, formatBytes, formatNumber } from '../../services/api'
import type {
  ClientRow,
  ProxyRow,
  ProxyWriteResponse,
  SettingsSnapshot,
  StatusSnapshot,
} from '../../types'
import type { ProxyDraft, ProxyRowView } from './types'

const proxies = ref<ProxyRow[]>([])
const clients = ref<ClientRow[]>([])
const socksBind = ref('')
const dialogOpen = ref(false)
const editing = ref<ProxyRow | null>(null)
const saving = ref(false)
const deleting = ref('')
const dialogError = ref('')
const updatedAt = ref('')
let noClientsNoticeShown = false
let noClientsNoticeId: number | null = null
let timer: number | undefined

const form = reactive<ProxyDraft>({
  run_id: '',
  name: '',
  type: 'tcp',
  local_addr: '127.0.0.1:8080',
  remote_port: 0,
  domainsText: '',
  max_conns: 0,
  idle_timeout_s: 0,
  rate_limit_bps: 0,
  allow_cidrs: '',
})

const search = reactive({ keyword: '', type: 'all' })
const applied = reactive({ keyword: '', type: 'all' })

function applySearch() {
  applied.keyword = search.keyword.trim().toLowerCase()
  applied.type = search.type
}

function resetSearch() {
  search.keyword = ''
  search.type = 'all'
  applySearch()
}

const clientOptions = computed(() =>
  clients.value.map((client) => ({
    value: client.run_id,
    label: `${client.client_id} · ${client.tenant_id}`,
    description: `${client.run_id.slice(0, 12)} · ${client.proxy_count} 个 proxy`,
  })),
)

const clientNames = computed(
  () => clients.value.map((client) => client.client_id).join(' · ') || '等待客户端连接',
)

const totalActive = computed(() =>
  proxies.value.reduce((sum, proxy) => sum + proxy.active_conns, 0),
)

const trafficTotal = computed(() => {
  const bytes = proxies.value.reduce(
    (sum, proxy) => sum + proxy.bytes_in + proxy.bytes_out,
    0,
  )
  const [value, unit] = formatBytes(bytes).split(' ')
  return { value, unit }
})

const summaryStats = computed(() => [
  {
    label: '注册 PROXY',
    value: proxies.value.length,
    unit: '条',
    hint: '按协议类型分布',
  },
  {
    label: '在线客户端',
    value: clients.value.length,
    unit: '台',
    hint: clientNames.value,
  },
  {
    label: '活跃连接',
    value: formatNumber(totalActive.value),
    unit: '条',
    hint: '当前转发中的 TCP 会话',
  },
  {
    label: '累计流量',
    value: trafficTotal.value.value,
    unit: trafficTotal.value.unit,
    hint: '入站 + 出站，按 proxy 汇总',
  },
])

function endpointOf(proxy: ProxyRow): string {
  if (proxy.type === 'socks5') {
    return `socks5://${socksBind.value || '0.0.0.0:2081'}`
  }
  if (proxy.type === 'http' || proxy.type === 'https') {
    return proxy.domains[0] ?? '—'
  }
  return `:${proxy.remote_port}`
}

const rows = computed<ProxyRowView[]>(() => {
  const keyword = applied.keyword
  return proxies.value
    .filter((proxy) => applied.type === 'all' || proxy.type === applied.type)
    .filter((proxy) => {
      if (!keyword) return true
      return [
        proxy.name,
        proxy.type,
        proxy.local_addr,
        proxy.domains.join(' '),
        String(proxy.remote_port),
        proxy.run_id,
      ]
        .join(' ')
        .toLowerCase()
        .includes(keyword)
    })
    .map((proxy) => ({
      ...proxy,
      key: `${proxy.run_id}-${proxy.name}`,
      endpoint: endpointOf(proxy),
      local_addr: proxy.type === 'socks5' ? '—' : proxy.local_addr || '—',
      domainsText: proxy.domains.length ? proxy.domains.join(', ') : '—',
      traffic: formatBytes(proxy.bytes_in + proxy.bytes_out),
    }))
})

function resetDialog() {
  dialogError.value = ''
  editing.value = null
}

function onDialogOpenChange(open: boolean) {
  dialogOpen.value = open
  if (!open) resetDialog()
}

function resetForm() {
  form.run_id = clients.value[0]?.run_id ?? ''
  form.name = ''
  form.type = 'tcp'
  form.local_addr = '127.0.0.1:8080'
  form.remote_port = 0
  form.domainsText = ''
  form.max_conns = 0
  form.idle_timeout_s = 0
  form.rate_limit_bps = 0
  form.allow_cidrs = ''
}

function openCreate() {
  resetForm()
  dialogError.value = ''
  editing.value = null
  dialogOpen.value = true
}

function openEdit(row: ProxyRow) {
  const proxy = row
  dialogError.value = ''
  editing.value = proxy
  form.run_id = proxy.run_id
  form.name = proxy.name
  form.type = proxy.type
  form.local_addr = proxy.local_addr
  form.remote_port = proxy.remote_port
  form.domainsText = proxy.domains.join(', ')
  form.max_conns = proxy.max_conns
  form.idle_timeout_s = proxy.idle_timeout_s
  form.rate_limit_bps = proxy.rate_limit_bps
  form.allow_cidrs = proxy.allow_cidrs.join(', ')
  dialogOpen.value = true
}

function parseList(value: string): string[] {
  return value
    .split(/[,，\s]+/)
    .map((item) => item.trim())
    .filter(Boolean)
}

function validateForm(): string {
  if (!form.run_id) return '请选择在线客户端'
  if (!/^[A-Za-z0-9_.-]{1,64}$/.test(form.name)) {
    return '名称只能包含字母、数字、点、下划线和短横线，长度 1–64'
  }
  if (!form.local_addr.trim()) return '本地服务地址不能为空'
  if (form.type === 'http' || form.type === 'https') {
    if (!parseList(form.domainsText).length) return 'HTTP / HTTPS 必须配置至少一个域名'
    if (form.remote_port !== 0) return 'HTTP / HTTPS 的远端端口必须为 0'
  } else if (form.type === 'socks5') {
    if (parseList(form.domainsText).length) return 'SOCKS5 不能配置自定义域名'
    if (form.remote_port !== 0) return 'SOCKS5 的远端端口必须为 0'
  } else if (parseList(form.domainsText).length) {
    return 'TCP / UDP 不能配置自定义域名'
  }
  return ''
}

async function submit() {
  const invalid = validateForm()
  if (invalid) {
    dialogError.value = invalid
    return
  }
  saving.value = true
  dialogError.value = ''
  try {
    const payload = {
      run_id: form.run_id,
      name: form.name.trim(),
      type: form.type,
      local_addr: form.local_addr.trim(),
      remote_port: form.remote_port,
      custom_domains: parseList(form.domainsText),
      options: {
        rate_limit_bps: form.rate_limit_bps,
        idle_timeout_s: form.idle_timeout_s,
        max_conns: form.max_conns,
        allow_cidrs: parseList(form.allow_cidrs),
      },
    }
    const result = await apiWrite<ProxyWriteResponse>(
      editing.value ? 'PUT' : 'POST',
      '/api/v1/proxies',
      payload,
    )
    notifySuccess(
      `${editing.value ? '已更新' : '已新增'} ${result.name}`,
      result.remote_port ? `入口端口 ${result.remote_port}` : '配置已下发到客户端',
    )
    dialogOpen.value = false
    editing.value = null
    await refresh()
  } catch (err) {
    dialogError.value = err instanceof Error ? err.message : String(err)
  } finally {
    saving.value = false
  }
}

async function removeProxy(row: ProxyRow) {
  deleting.value = row.name
  try {
    await apiWrite('DELETE', '/api/v1/proxies', {
      run_id: row.run_id,
      name: row.name,
    })
    notifySuccess(`已删除 ${row.name}`, '客户端已停止并移除该监听。')
    await refresh()
  } catch (err) {
    notifyError('操作失败', err instanceof Error ? err.message : String(err))
  } finally {
    deleting.value = ''
  }
}

async function refresh() {
  try {
    const data = await apiGet<StatusSnapshot>('/api/v1/status')
    proxies.value = data.proxies
    clients.value = data.clients
    updatedAt.value = new Date().toLocaleTimeString('zh-CN', { hour12: false })
    if (!data.clients.length && !noClientsNoticeShown) {
      noClientsNoticeShown = true
      noClientsNoticeId = notifyWarning(
        '没有在线客户端',
        'proxy 由客户端实际监听本地服务。请先启动 meatus client，待其连上 server 后再新增或编辑。',
      )
    } else if (data.clients.length) {
      noClientsNoticeShown = false
      if (noClientsNoticeId !== null) {
        dismissNotification(noClientsNoticeId)
        noClientsNoticeId = null
      }
    }
    if (!editing.value && dialogOpen.value && !form.run_id) {
      form.run_id = data.clients[0]?.run_id ?? ''
    }
  } catch {
    // 总览与登录态会提示；这里保持上一帧，避免页面闪空。
  }
}

async function copyAddress(row: ProxyRow) {
  const serverHost = window.location.hostname || '127.0.0.1'
  const address = row.domains[0]
    ? `${row.type === 'https' ? 'https' : 'http'}://${row.domains[0]}`
    : row.type === 'socks5'
      ? `socks5://${socksBind.value || `${serverHost}:2081`}`
      : `${row.type}://${serverHost}:${row.remote_port}`
  try {
    await navigator.clipboard.writeText(address)
    notifySuccess('地址已复制', address)
  } catch {
    // 非安全上下文下 clipboard 不可用。
  }
}

onMounted(() => {
  refresh()
  apiGet<SettingsSnapshot>('/api/v1/settings')
    .then((settings) => {
      socksBind.value = settings.socks5.bind
    })
    .catch(() => {
      socksBind.value = ''
    })
  timer = window.setInterval(refresh, 5000)
})
onBeforeUnmount(() => {
  if (timer) window.clearInterval(timer)
  if (noClientsNoticeId !== null) dismissNotification(noClientsNoticeId)
})
</script>
