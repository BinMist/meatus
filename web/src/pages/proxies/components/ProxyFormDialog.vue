<template>
  <sl-dialog
    v-model:open="openModel"
    :aria-label="editing ? '编辑 proxy' : '新增 proxy'"
    width="max(360px, 50vw)"
    :loading="saving"
    :confirm-disabled="saving"
    confirm-label="保存"
    @confirm="emit('confirm')"
    @cancel="emit('cancel')"
  >
    <template #header>
      <div class="sl-dialog__head-copy mt-dialog-head">
        <h2 class="sl-dialog__title">{{ editing ? '编辑 proxy' : '新增 proxy' }}</h2>
        <HintTip
          placement="bottom"
          :focusable="false"
          content="名称保存后不可修改；保存即下发客户端并写回 client.toml，无需重启。"
        />
      </div>
    </template>

    <sl-form :model="form" label-position="top" @submit.prevent>
      <div class="mt-form-grid mt-form-grid--four mt-form-grid--compact">
        <sl-form-item label="客户端" prop="run_id" required>
          <template #label="{ label, required }">
            <FormLabel
              :label="label"
              :required="required"
              hint="只列出当前在线客户端；离线客户端需先启动 meatus client。"
            />
          </template>
          <sl-select
            v-model:value="form.run_id"
            :options="clientOptions"
            placeholder="选择在线客户端"
            :disabled="Boolean(editing)"
          />
        </sl-form-item>

        <sl-form-item label="名称" prop="name" required>
          <template #label="{ label, required }">
            <FormLabel
              :label="label"
              :required="required"
              hint="保存后不可重命名；如需改名，请删除后重新创建。"
            />
          </template>
          <sl-input
            v-model:value="form.name"
            placeholder="例如 nas-web"
            :disabled="Boolean(editing)"
            maxlength="64"
          />
        </sl-form-item>

        <sl-form-item label="类型" prop="type" required>
          <template #label="{ label, required }">
            <FormLabel
              :label="label"
              :required="required"
              hint="TCP / UDP 走端口映射；HTTP / HTTPS 按域名路由；SOCKS5 复用 server 共享监听。"
            />
          </template>
          <sl-select v-model:value="form.type" :options="proxyTypeOptions" />
        </sl-form-item>

        <sl-form-item label="本地服务" prop="local_addr" required>
          <template #label="{ label, required }">
            <FormLabel
              :label="label"
              :required="required"
              hint="客户端本机实际监听的地址，例如 127.0.0.1:8080。"
            />
          </template>
          <sl-input v-model:value="form.local_addr" placeholder="127.0.0.1:8080" />
        </sl-form-item>

        <sl-form-item label="远端端口" prop="remote_port">
          <template #label="{ label }">
            <FormLabel :label="label" :hint="remotePortHint" />
          </template>
          <sl-number-field
            v-model:value="form.remote_port"
            :min="0"
            :max="65535"
            :format-options="{ useGrouping: false }"
          />
        </sl-form-item>

        <sl-form-item
          v-if="form.type === 'http' || form.type === 'https'"
          label="自定义域名"
          prop="domainsText"
          required
        >
          <template #label="{ label, required }">
            <FormLabel
              :label="label"
              :required="required"
              hint="多个域名用英文逗号分隔，最多 16 个。"
            />
          </template>
          <sl-input v-model:value="form.domainsText" placeholder="app.example.com, www.example.com" />
        </sl-form-item>
      </div>

      <sl-alert
        v-if="error"
        class="mt-dialog-error"
        type="error"
        title="请检查表单"
        :description="error"
        variant="subtle"
        show-icon
      />

      <div class="mt-form-section mt-form-section--tight">高级选项</div>
      <div class="mt-form-grid mt-form-grid--four mt-form-grid--compact">
        <sl-form-item label="最大连接数">
          <template #label="{ label }">
            <FormLabel :label="label" hint="0 表示不限制该 proxy 的并发连接数。" />
          </template>
          <sl-number-field v-model:value="form.max_conns" :min="0" :max="4096">
            <template #suffix>个</template>
          </sl-number-field>
        </sl-form-item>
        <sl-form-item label="空闲超时">
          <template #label="{ label }">
            <FormLabel :label="label" hint="0 表示沿用 server 默认的空闲连接超时。" />
          </template>
          <sl-number-field v-model:value="form.idle_timeout_s" :min="0" :max="3600">
            <template #suffix>秒</template>
          </sl-number-field>
        </sl-form-item>
        <sl-form-item label="限速">
          <template #label="{ label }">
            <FormLabel :label="label" hint="单 proxy 的出站带宽上限；0 表示不限制。" />
          </template>
          <sl-number-field v-model:value="form.rate_limit_bps" :min="0">
            <template #suffix>B/s</template>
          </sl-number-field>
        </sl-form-item>
        <sl-form-item v-if="form.type === 'socks5'" label="允许目标 CIDR">
          <template #label="{ label }">
            <FormLabel
              :label="label"
              hint="仅 SOCKS5 生效；留空表示不限制，多个 CIDR 用英文逗号分隔。"
            />
          </template>
          <sl-input v-model:value="form.allow_cidrs" placeholder="10.0.0.0/8, 192.168.1.0/24" />
        </sl-form-item>
      </div>
    </sl-form>
  </sl-dialog>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { SlAlert } from '@sailonix/ui/alert'
import { SlDialog } from '@sailonix/ui/dialog'
import { SlForm, SlFormItem } from '@sailonix/ui/form'
import { SlInput } from '@sailonix/ui/input'
import { SlNumberField } from '@sailonix/ui/number-field'
import { SlSelect } from '@sailonix/ui/select'
import FormLabel from '../../../components/FormLabel.vue'
import HintTip from '../../../components/HintTip.vue'
import { proxyTypeOptions } from '../constants'
import type { ProxyRow } from '../../../types'
import type { ProxyDraft } from '../types'

defineOptions({ name: 'ProxyFormDialog' })

const props = defineProps<{
  open: boolean
  editing: ProxyRow | null
  form: ProxyDraft
  clientOptions: Array<{ value: string; label: string; description: string }>
  error: string
  saving: boolean
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
  confirm: []
  cancel: []
}>()

const openModel = computed({
  get: () => props.open,
  set: (value: boolean) => emit('update:open', value),
})

const remotePortHint = computed(() =>
  props.form.type === 'tcp' || props.form.type === 'udp'
    ? '0 = 由 server 端口池自动分配；公网访问还需在宿主机 Docker 发布该端口，并在云安全组放行。'
    : '0 = 由 server 端口池自动分配。',
)
</script>
