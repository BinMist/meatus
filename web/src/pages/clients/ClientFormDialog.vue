<template>
  <sl-dialog
    v-model:open="openModel"
    :aria-label="mode === 'create' ? '新增客户端' : '轮换 token'"
    width="min(760px, 92vw)"
    :loading="saving"
    :show-footer="!result"
    :confirm-label="mode === 'create' ? '创建并生成 token' : '轮换 token'"
    :confirm-disabled="saving"
    @confirm="emit('confirm')"
    @cancel="emit('cancel')"
  >
    <template #header>
      <div class="sl-dialog__head-copy mt-dialog-head">
        <h2 class="sl-dialog__title">
          {{ result ? '客户端凭据已生成' : mode === 'create' ? '新增客户端' : '轮换客户端 token' }}
        </h2>
        <HintTip
          placement="bottom"
          :focusable="false"
          :content="
            result
              ? '明文 token 只显示这一次，关闭后无法再次查看。'
              : '保存后写入服务端配置，并在 1 秒后重启；客户端需要更新 token 后重新连接。'
          "
        />
      </div>
    </template>

    <template v-if="result">
      <sl-alert
        type="warning"
        title="请立即保存"
        description="token 只在本窗口显示一次。关闭后只能轮换，不能找回。"
        variant="subtle"
        show-icon
      />

      <div class="mt-token-row">
        <div>
          <label class="mt-token-label">Client token</label>
          <pre class="mt-token-box">{{ result.token }}</pre>
        </div>
        <sl-button size="sm" @click="emit('copy', result.token || '')">
          <template #leading><sl-icon name="copy" :size="14" /></template>
          复制 token
        </sl-button>
      </div>

      <div class="mt-config-block">
        <div class="mt-config-head">
          <span>client.toml</span>
          <sl-button type="ghost" size="xs" @click="emit('copy', result.config?.toml || '')">
            复制 TOML
          </sl-button>
        </div>
        <pre>{{ result.config?.toml }}</pre>
      </div>

      <div class="mt-config-block">
        <div class="mt-config-head">
          <span>client.yaml</span>
          <sl-button type="ghost" size="xs" @click="emit('copy', result.config?.yaml || '')">
            复制 YAML
          </sl-button>
        </div>
        <pre>{{ result.config?.yaml }}</pre>
      </div>

      <div class="mt-config-foot">
        写入 {{ result.message || '服务端配置' }}。请把 token 填到客户端配置后执行
        <code>meatus check -c client.toml</code>。
      </div>
    </template>

    <sl-form v-else :model="form" label-position="top" @submit.prevent>
      <div class="mt-form-grid mt-form-grid--two">
        <sl-form-item label="客户端 ID" prop="id" required>
          <template #label="{ label, required }">
            <FormLabel :label="label" :required="required" hint="保存后不可修改，建议使用主机名或用途，例如 home-nas。" />
          </template>
          <sl-input
            v-model:value="form.id"
            placeholder="home-nas"
            maxlength="64"
            :disabled="mode === 'rotate'"
          />
        </sl-form-item>

        <sl-form-item label="租户" prop="tenant">
          <template #label="{ label }">
            <FormLabel :label="label" hint="默认 default；多租户场景再填写对应 tenant。" />
          </template>
          <sl-input
            v-model:value="form.tenant"
            placeholder="default"
            maxlength="64"
            :disabled="mode === 'rotate'"
          />
        </sl-form-item>

        <sl-form-item label="服务端地址" prop="serverAddr">
          <template #label="{ label }">
            <FormLabel label="服务端地址" hint="生成配置里的 server_addr，格式 host:port，不带 http://。" />
          </template>
          <sl-input v-model:value="form.serverAddr" placeholder="tunnel.example.com:7000" />
        </sl-form-item>

        <sl-form-item label="允许端口范围" prop="allowPorts">
          <template #label="{ label }">
            <FormLabel :label="label" hint="可留空；填写时使用 start-end，例如 20000-21000。" />
          </template>
          <sl-input v-model:value="form.allowPorts" placeholder="20000-21000（可留空）" />
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
    </sl-form>

    <template v-if="result" #footer>
      <sl-button @click="emit('cancel')">我已保存，关闭</sl-button>
    </template>
  </sl-dialog>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { SlAlert } from '@sailonix/ui/alert'
import { SlButton } from '@sailonix/ui/button'
import { SlDialog } from '@sailonix/ui/dialog'
import { SlForm, SlFormItem } from '@sailonix/ui/form'
import { SlInput } from '@sailonix/ui/input'
import { SlIcon } from '@sailonix/icon'
import FormLabel from '../../components/FormLabel.vue'
import HintTip from '../../components/HintTip.vue'
import type { ClientRow, ClientWriteResponse } from './types'

defineOptions({ name: 'ClientFormDialog' })

const props = defineProps<{
  open: boolean
  mode: 'create' | 'rotate'
  client: ClientRow | null
  form: { id: string; tenant: string; serverAddr: string; allowPorts: string }
  error: string
  saving: boolean
  result: ClientWriteResponse | null
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
  confirm: []
  cancel: []
  copy: [text: string]
}>()

const openModel = computed({
  get: () => props.open,
  set: (value: boolean) => emit('update:open', value),
})
</script>
