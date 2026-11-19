<template>
  <div class="page page--tokens">
    <sl-page-header
      title="API Token"
      sub-title="面向 CI / 自动化脚本的短期凭证；服务端只保存 SHA-256，明文只在创建时返回一次"
    >
      <template #extra>
        <PageActionsBar
          :meta="updatedAt ? `更新于 ${updatedAt}` : '等待采样'"
          @refresh="refresh"
        />
      </template>
    </sl-page-header>

    <sl-alert
      type="info"
      title="最小权限"
      description="每个 Token 只能访问所选 Scope；API Token 不能管理其他 Token，也不能调用配置写回。吊销或过期后立即失效。"
      variant="subtle"
      show-icon
    />

    <div class="mt-token-grid">
      <sl-panel title="新建 API Token" description="名称用于审计与吊销；建议按用途命名">
        <div class="mt-token-form">
          <sl-form-item label="名称" help="1–64 位 ASCII 字母、数字、-、_ 或 .">
            <sl-input v-model:value="draft.name" placeholder="ci-deploy" clearable />
          </sl-form-item>
          <sl-form-item label="有效期（天）" help="0 表示长期有效">
            <sl-number-field
              v-model:value="draft.expiresDays"
              :min="0"
              :max="3650"
              :format-options="{ useGrouping: false }"
            />
          </sl-form-item>
          <sl-form-item label="Scope" help="至少选择一个">
            <div class="mt-token-scopes">
              <sl-checkbox-card
                v-model="draft.scopes.statusRead"
                title="status:read"
                description="读取 /status 与总览指标"
                size="sm"
              />
              <sl-checkbox-card
                v-model="draft.scopes.proxiesWrite"
                title="proxies:write"
                description="创建、修改、删除 proxy 与探测"
                size="sm"
              />
              <sl-checkbox-card
                v-model="draft.scopes.certsRead"
                title="certs:read"
                description="读取证书状态"
                size="sm"
              />
              <sl-checkbox-card
                v-model="draft.scopes.auditRead"
                title="audit:read"
                description="读取审计事件"
                size="sm"
              />
            </div>
          </sl-form-item>
          <sl-button
            type="primary"
            size="sm"
            :loading="creating"
            :disabled="!canCreate"
            @click="createToken"
          >
            <template #leading><sl-icon name="shield-check" :size="15" /></template>
            创建 Token
          </sl-button>
        </div>
      </sl-panel>

      <sl-panel title="Token 概览" description="指纹用于核对，不是可恢复的凭证">
        <dl class="mt-token-facts">
          <div>
            <dt>Token 总数</dt>
            <dd>{{ rows.length }}</dd>
          </div>
          <div>
            <dt>有效</dt>
            <dd>{{ activeCount }}</dd>
          </div>
          <div>
            <dt>已过期</dt>
            <dd>{{ expiredCount }}</dd>
          </div>
          <div>
            <dt>最近使用</dt>
            <dd>{{ latestUsed }}</dd>
          </div>
        </dl>
      </sl-panel>
    </div>

    <sl-panel :description="`${rows.length} 个 Token`" padding="none">
      <sl-data-table
        :columns="columns"
        :data="rows"
        row-key="name"
        caption="API Token"
        unit="个"
        empty-text="还没有 API Token；在上方创建第一个"
        :page-size="20"
        :searchable="false"
        dense
      >
        <template #cell="{ column, row }">
          <div v-if="column.key === 'name'" class="mt-token-name">
            <span>{{ row.name }}</span>
            <span class="mt-token-name__hint">指纹 {{ row.fingerprint }}</span>
          </div>
          <div v-else-if="column.key === 'scopes'" class="mt-token-scope-list">
            <sl-tag v-for="scope in row.scopes" :key="scope" size="sm" variant="subtle">{{ scope }}</sl-tag>
          </div>
          <span v-else-if="column.key === 'created'" class="mt-token-time">{{ row.createdText }}</span>
          <span v-else-if="column.key === 'expires'" class="mt-token-time">{{ row.expiresText }}</span>
          <span v-else-if="column.key === 'last_used'" class="mt-token-time">{{ row.lastUsedText }}</span>
          <span v-else-if="column.key === 'status'">
            <sl-tag size="sm" variant="subtle" :class="row.expired ? 'mt-token-expired' : 'mt-token-active'">
              {{ row.expired ? '已过期' : '有效' }}
            </sl-tag>
          </span>
          <span v-else-if="column.key === 'actions'" class="mt-token-actions">
            <sl-button type="ghost" size="sm" @click="askRevoke(String(row.name))">吊销</sl-button>
          </span>
          <template v-else>{{ row[column.key] }}</template>
        </template>
      </sl-data-table>
    </sl-panel>

    <sl-dialog
      v-model:open="plaintextOpen"
      aria-label="API Token 已创建"
      width="min(560px, 92vw)"
      confirm-label="我已保存"
      @confirm="plaintextOpen = false"
      @cancel="plaintextOpen = false"
    >
      <template #header>
        <h2 class="sl-dialog__title">API Token 已创建</h2>
      </template>
      <p class="mt-token-once">
        这是唯一一次展示明文。关闭后无法再次查看，只能重新创建。不要粘贴到聊天、日志或工单。
      </p>
      <div class="mt-token-plaintext">
        <code>{{ createdToken }}</code>
        <sl-button type="secondary" size="sm" @click="copyToken">复制</sl-button>
      </div>
      <p class="mt-token-once">名称：{{ createdName }} · Scope：{{ createdScopes.join('、') }}</p>
    </sl-dialog>

    <sl-dialog
      v-model:open="revokeOpen"
      aria-label="吊销 API Token"
      width="min(440px, 92vw)"
      confirm-label="吊销"
      @confirm="revoke"
      @cancel="revokeOpen = false"
    >
      <template #header>
        <h2 class="sl-dialog__title">吊销 {{ pendingRevoke }}？</h2>
      </template>
      <p class="mt-token-once">吊销后该 Token 会在热加载完成后立即返回 401，且无法恢复。</p>
    </sl-dialog>
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onMounted, reactive, ref } from 'vue'
import { SlAlert } from '@sailonix/ui/alert'
import { SlButton } from '@sailonix/ui/button'
import { SlCheckboxCard } from '@sailonix/ui/checkbox'
import { SlDataTable } from '@sailonix/ui/data-table'
import type { DataTableColumn } from '@sailonix/ui/data-table'
import { SlDialog } from '@sailonix/ui/dialog'
import { SlFormItem } from '@sailonix/ui/form'
import { SlIcon } from '@sailonix/icon'
import { SlInput } from '@sailonix/ui/input'
import { SlNumberField } from '@sailonix/ui/number-field'
import { SlPageHeader } from '@sailonix/ui/page-header'
import { SlPanel } from '@sailonix/ui/panel'
import { SlTag } from '@sailonix/ui/tag'
import PageActionsBar from '../../components/PageActionsBar.vue'
import { apiGet, apiWrite, formatTime } from '../../services/api'
import { notifyError, notifySuccess } from '../../services/notifications'

interface ApiTokenRow {
  name: string
  scopes: string[]
  created_at_ms: number | null
  expires_at_ms: number | null
  expired: boolean
  fingerprint: string
  last_used_at_ms: number | null
  createdText: string
  expiresText: string
  lastUsedText: string
}

interface TokenList {
  tokens: ApiTokenRow[]
}

interface CreateTokenResponse {
  ok: boolean
  token: string
  name: string
  scopes: string[]
  fingerprint: string
}

const rows = ref<ApiTokenRow[]>([])
const updatedAt = ref('')
const creating = ref(false)
const plaintextOpen = ref(false)
const createdToken = ref('')
const createdName = ref('')
const createdScopes = ref<string[]>([])
const revokeOpen = ref(false)
const pendingRevoke = ref('')

const draft = reactive({
  name: '',
  expiresDays: 0,
  scopes: {
    statusRead: true,
    proxiesWrite: false,
    certsRead: false,
    auditRead: false,
  },
})

const scopeMap: Record<string, string> = {
  statusRead: 'status:read',
  proxiesWrite: 'proxies:write',
  certsRead: 'certs:read',
  auditRead: 'audit:read',
}

const selectedScopes = computed(() =>
  Object.entries(draft.scopes)
    .filter(([, enabled]) => enabled)
    .map(([key]) => scopeMap[key])
    .filter(Boolean),
)

const canCreate = computed(() => draft.name.trim().length > 0 && selectedScopes.value.length > 0)
const activeCount = computed(() => rows.value.filter((row) => !row.expired).length)
const expiredCount = computed(() => rows.value.filter((row) => row.expired).length)
const latestUsed = computed(() => {
  const latest = rows.value.reduce<number | null>((current, row) => {
    if (!row.last_used_at_ms) return current
    return current === null ? row.last_used_at_ms : Math.max(current, row.last_used_at_ms)
  }, null)
  return latest ? formatTime(latest) : '尚未使用'
})

const columns: DataTableColumn[] = [
  { key: 'name', header: '名称', type: 'text', width: 190 },
  { key: 'scopes', header: 'Scope', type: 'text', width: 240 },
  { key: 'created', header: '创建时间', type: 'text', width: 145 },
  { key: 'expires', header: '到期时间', type: 'text', width: 145 },
  { key: 'last_used', header: '最后使用', type: 'text', width: 145 },
  { key: 'status', header: '状态', type: 'text', width: 82 },
  { key: 'actions', header: '操作', type: 'text', width: 80 },
]

async function refresh() {
  try {
    const data = await apiGet<TokenList>('/api/v1/api-tokens')
    rows.value = data.tokens.map((token) => ({
      ...token,
      createdText: formatTime(token.created_at_ms),
      expiresText: token.expires_at_ms ? formatTime(token.expires_at_ms) : '长期有效',
      lastUsedText: token.last_used_at_ms ? formatTime(token.last_used_at_ms) : '尚未使用',
    }))
    updatedAt.value = new Date().toLocaleTimeString('zh-CN', { hour12: false })
  } catch (error) {
    notifyError('读取 API Token 失败', String(error))
  }
}

async function createToken() {
  if (!canCreate.value) return
  creating.value = true
  try {
    const expiresAt =
      draft.expiresDays > 0 ? Date.now() + Math.round(draft.expiresDays) * 86_400_000 : undefined
    const response = await apiWrite<CreateTokenResponse>('POST', '/api/v1/api-tokens', {
      name: draft.name.trim(),
      scopes: selectedScopes.value,
      ...(expiresAt ? { expires_at_ms: expiresAt } : {}),
    })
    createdToken.value = response.token
    createdName.value = response.name
    createdScopes.value = response.scopes
    plaintextOpen.value = true
    draft.name = ''
    await refresh()
    notifySuccess('API Token 已创建', `指纹 ${response.fingerprint}`)
  } catch (error) {
    notifyError('创建失败', String(error))
  } finally {
    creating.value = false
  }
}

async function copyToken() {
  try {
    await navigator.clipboard.writeText(createdToken.value)
    notifySuccess('已复制明文', '请立即写入 CI Secret，关闭后无法再次查看')
  } catch {
    notifyError('复制失败', '请手动选中复制')
  }
}

function askRevoke(name: string) {
  pendingRevoke.value = name
  revokeOpen.value = true
}

async function revoke() {
  const name = pendingRevoke.value
  revokeOpen.value = false
  if (!name) return
  try {
    await apiWrite('DELETE', `/api/v1/api-tokens/${encodeURIComponent(name)}`)
    notifySuccess('Token 已吊销', name)
    await refresh()
  } catch (error) {
    notifyError('吊销失败', String(error))
  }
}

onMounted(refresh)
</script>
