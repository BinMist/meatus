<template>
  <sl-dialog
    v-model:open="openModel"
    aria-label="保存配置并热加载？"
    width="max(360px, 50vw)"
    :loading="saving"
    :confirm-disabled="saving"
    confirm-label="保存配置"
    @confirm="emit('confirm')"
    @cancel="emit('cancel')"
  >
    <template #header>
      <div class="sl-dialog__head-copy mt-dialog-head">
        <h2 class="sl-dialog__title">保存配置并热加载？</h2>
        <HintTip
          placement="bottom"
          :focusable="false"
          content="保存后立即热加载 auth / 日志；监听地址、TLS 证书等变更会提示需重启。校验或端口预检失败时不会写入。"
        />
      </div>
    </template>
    <p class="mt-dialog-summary">写入 <code>server.toml</code> 并立即热加载；需重启字段只提示，不自动重启</p>
  </sl-dialog>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { SlDialog } from '@sailonix/ui/dialog'
import HintTip from '../../../components/HintTip.vue'

defineOptions({ name: 'SettingsSaveDialog' })

const props = defineProps<{ open: boolean; saving: boolean }>()
const emit = defineEmits<{
  'update:open': [value: boolean]
  confirm: []
  cancel: []
}>()

const openModel = computed({
  get: () => props.open,
  set: (value: boolean) => emit('update:open', value),
})
</script>
