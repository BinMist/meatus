<template>
  <sl-dialog
    v-model:open="openModel"
    aria-label="保存配置并重启 server？"
    width="max(360px, 50vw)"
    :loading="saving"
    :confirm-disabled="saving || restarting"
    confirm-label="保存并重启"
    @confirm="emit('confirm')"
    @cancel="emit('cancel')"
  >
    <template #header>
      <div class="sl-dialog__head-copy mt-dialog-head">
        <h2 class="sl-dialog__title">保存配置并重启 server？</h2>
        <HintTip
          placement="bottom"
          :focusable="false"
          content="保存后 server 重启，面板通常中断 3–10 秒；校验或端口预检失败时不会写入。"
        />
      </div>
    </template>
    <p class="mt-dialog-summary">写入 <code>server.toml</code> 并重启 server</p>
  </sl-dialog>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { SlDialog } from '@sailonix/ui/dialog'
import HintTip from '../../../components/HintTip.vue'

defineOptions({ name: 'SettingsSaveDialog' })

const props = defineProps<{ open: boolean; saving: boolean; restarting: boolean }>()
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
